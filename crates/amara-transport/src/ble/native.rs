//! The host's Bluetooth stack: central role through `btleplug`, peripheral role
//! through `ble-peripheral-rust` (BlueZ on Linux/Pi, WinRT on Windows,
//! CoreBluetooth on macOS).

use super::{
    BleRadio, DEFAULT_WRITE_SIZE, LinkId, RX_CHAR_UUID, RadioEvent, SERVICE_UUID, TX_CHAR_UUID,
};
use amara_core::{AmaraError, Result};
use async_trait::async_trait;
use ble_peripheral_rust::gatt::characteristic::Characteristic as GattCharacteristic;
use ble_peripheral_rust::gatt::peripheral_event::{
    PeripheralEvent, ReadRequestResponse, RequestResponse, WriteRequestResponse,
};
use ble_peripheral_rust::gatt::properties::{AttributePermission, CharacteristicProperty};
use ble_peripheral_rust::gatt::service::Service;
use ble_peripheral_rust::{Peripheral as GattServer, PeripheralImpl};
use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The link id that stands for "every central subscribed to our TX characteristic".
const PERIPHERAL_LINK: LinkId = 0;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which BLE roles to run.
#[derive(Debug, Clone)]
pub struct NativeRadioConfig {
    /// Advertised local name.
    pub name: String,
    /// Scan for and connect to other AMARA nodes.
    pub central: bool,
    /// Advertise the AMARA service and accept connections.
    pub peripheral: bool,
    /// Bytes per GATT write/notification.
    pub write_size: usize,
}

impl Default for NativeRadioConfig {
    fn default() -> Self {
        Self {
            name: "amara".into(),
            central: true,
            peripheral: true,
            write_size: DEFAULT_WRITE_SIZE,
        }
    }
}

struct CentralLink {
    device: Peripheral,
    rx: Characteristic,
}

#[derive(Default)]
struct State {
    /// Connections we made (central role).
    links: HashMap<LinkId, CentralLink>,
    by_device: HashMap<PeripheralId, LinkId>,
    connecting: HashSet<PeripheralId>,
    /// Centrals subscribed to us (peripheral role).
    subscribers: HashSet<String>,
}

/// The host Bluetooth adapter as a [`BleRadio`].
pub struct NativeRadio {
    config: NativeRadioConfig,
    state: Arc<Mutex<State>>,
    server: Arc<AsyncMutex<Option<GattServer>>>,
    next_link: Arc<AtomicU64>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl NativeRadio {
    pub fn new(config: NativeRadioConfig) -> Self {
        Self {
            config,
            state: Arc::default(),
            server: Arc::default(),
            next_link: Arc::new(AtomicU64::new(PERIPHERAL_LINK + 1)),
            tasks: Mutex::new(Vec::new()),
        }
    }

    async fn start_peripheral(&self, events: mpsc::Sender<RadioEvent>) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<PeripheralEvent>(256);
        let mut server = GattServer::new(tx)
            .await
            .map_err(|e| AmaraError::Ble(format!("peripheral: {e}")))?;
        let powered = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if server.is_powered().await.unwrap_or(false) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await;
        if powered.is_err() {
            return Err(AmaraError::Ble("Bluetooth is off".into()));
        }
        let service = Uuid::from_u128(SERVICE_UUID);
        server
            .add_service(&Service {
                uuid: service,
                primary: true,
                characteristics: vec![
                    GattCharacteristic {
                        uuid: Uuid::from_u128(RX_CHAR_UUID),
                        properties: vec![
                            CharacteristicProperty::Write,
                            CharacteristicProperty::WriteWithoutResponse,
                        ],
                        permissions: vec![AttributePermission::Writeable],
                        ..Default::default()
                    },
                    GattCharacteristic {
                        uuid: Uuid::from_u128(TX_CHAR_UUID),
                        properties: vec![
                            CharacteristicProperty::Read,
                            CharacteristicProperty::Notify,
                        ],
                        permissions: vec![AttributePermission::Readable],
                        ..Default::default()
                    },
                ],
            })
            .await
            .map_err(|e| AmaraError::Ble(format!("add service: {e}")))?;
        server
            .start_advertising(&self.config.name, &[service])
            .await
            .map_err(|e| AmaraError::Ble(format!("advertise: {e}")))?;
        *self.server.lock().await = Some(server);
        info!("BLE peripheral advertising as '{}'", self.config.name);

        let state = self.state.clone();
        let handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    PeripheralEvent::WriteRequest {
                        request,
                        value,
                        responder,
                        ..
                    } => {
                        let _ = responder.send(WriteRequestResponse {
                            response: RequestResponse::Success,
                        });
                        if request.characteristic == Uuid::from_u128(RX_CHAR_UUID) {
                            let _ = events.send(RadioEvent::Data(PERIPHERAL_LINK, value)).await;
                        }
                    }
                    PeripheralEvent::ReadRequest { responder, .. } => {
                        let _ = responder.send(ReadRequestResponse {
                            value: Vec::new(),
                            response: RequestResponse::Success,
                        });
                    }
                    PeripheralEvent::CharacteristicSubscriptionUpdate {
                        request,
                        subscribed,
                    } => {
                        let now_empty = {
                            let mut s = state.lock().unwrap();
                            if subscribed {
                                s.subscribers.insert(request.client);
                            } else {
                                s.subscribers.remove(&request.client);
                            }
                            s.subscribers.is_empty()
                        };
                        let event = match (subscribed, now_empty) {
                            (true, _) => Some(RadioEvent::Up(PERIPHERAL_LINK)),
                            (false, true) => Some(RadioEvent::Down(PERIPHERAL_LINK)),
                            _ => None,
                        };
                        if let Some(event) = event {
                            let _ = events.send(event).await;
                        }
                    }
                    PeripheralEvent::StateUpdate { is_powered } => {
                        debug!("BLE peripheral powered: {is_powered}")
                    }
                }
            }
        });
        self.tasks.lock().unwrap().push(handle);
        Ok(())
    }

    async fn start_central(&self, events: mpsc::Sender<RadioEvent>) -> Result<()> {
        let manager = Manager::new()
            .await
            .map_err(|e| AmaraError::Ble(format!("central: {e}")))?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|e| AmaraError::Ble(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| AmaraError::Ble("no Bluetooth adapter".into()))?;
        let mut stream = adapter
            .events()
            .await
            .map_err(|e| AmaraError::Ble(e.to_string()))?;
        adapter
            .start_scan(ScanFilter {
                services: vec![Uuid::from_u128(SERVICE_UUID)],
            })
            .await
            .map_err(|e| AmaraError::Ble(format!("scan: {e}")))?;
        info!("BLE central scanning for AMARA nodes");

        let state = self.state.clone();
        let next_link = self.next_link.clone();
        let handle = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                match event {
                    CentralEvent::DeviceDiscovered(id)
                    | CentralEvent::ServicesAdvertisement { id, .. } => {
                        let fresh = {
                            let mut s = state.lock().unwrap();
                            !s.by_device.contains_key(&id) && s.connecting.insert(id.clone())
                        };
                        if fresh {
                            tokio::spawn(connect(
                                adapter.clone(),
                                id,
                                state.clone(),
                                next_link.clone(),
                                events.clone(),
                            ));
                        }
                    }
                    CentralEvent::DeviceDisconnected(id) => {
                        let link = {
                            let mut s = state.lock().unwrap();
                            let link = s.by_device.remove(&id);
                            if let Some(link) = link {
                                s.links.remove(&link);
                            }
                            link
                        };
                        if let Some(link) = link {
                            let _ = events.send(RadioEvent::Down(link)).await;
                        }
                    }
                    _ => {}
                }
            }
        });
        self.tasks.lock().unwrap().push(handle);
        Ok(())
    }
}

/// Connect to one discovered AMARA peripheral and pump its notifications.
async fn connect(
    adapter: Adapter,
    id: PeripheralId,
    state: Arc<Mutex<State>>,
    next_link: Arc<AtomicU64>,
    events: mpsc::Sender<RadioEvent>,
) {
    let result = async {
        let device = adapter.peripheral(&id).await?;
        if !device.is_connected().await? {
            device.connect_with_timeout(CONNECT_TIMEOUT).await?;
        }
        device
            .discover_services_with_timeout(CONNECT_TIMEOUT)
            .await?;
        let chars = device.characteristics();
        let find = |uuid: u128| {
            chars
                .iter()
                .find(|c| c.uuid == Uuid::from_u128(uuid))
                .cloned()
        };
        let (Some(rx), Some(tx)) = (find(RX_CHAR_UUID), find(TX_CHAR_UUID)) else {
            return Ok(None); // not an AMARA node after all
        };
        device.subscribe(&tx).await?;
        let notifications = device.notifications().await?;
        Ok::<_, btleplug::Error>(Some((device, rx, notifications)))
    }
    .await;
    state.lock().unwrap().connecting.remove(&id);
    let (device, rx, mut notifications) = match result {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => return debug!("BLE connect to {id:?} failed: {e}"),
    };
    let link = next_link.fetch_add(1, Ordering::Relaxed);
    {
        let mut s = state.lock().unwrap();
        s.by_device.insert(id.clone(), link);
        s.links.insert(link, CentralLink { device, rx });
    }
    debug!("BLE link {link} up to {id:?}");
    let _ = events.send(RadioEvent::Up(link)).await;
    while let Some(n) = notifications.next().await {
        if n.uuid == Uuid::from_u128(TX_CHAR_UUID) {
            let _ = events.send(RadioEvent::Data(link, n.value)).await;
        }
    }
    let removed = {
        let mut s = state.lock().unwrap();
        s.by_device.remove(&id);
        s.links.remove(&link).is_some()
    };
    if removed {
        let _ = events.send(RadioEvent::Down(link)).await;
    }
}

#[async_trait]
impl BleRadio for NativeRadio {
    async fn start(&self, events: mpsc::Sender<RadioEvent>) -> Result<()> {
        let mut started = false;
        if self.config.peripheral {
            match self.start_peripheral(events.clone()).await {
                Ok(()) => started = true,
                Err(e) => warn!("BLE peripheral role unavailable: {e}"),
            }
        }
        if self.config.central {
            match self.start_central(events).await {
                Ok(()) => started = true,
                Err(e) => warn!("BLE central role unavailable: {e}"),
            }
        }
        if started {
            Ok(())
        } else {
            Err(AmaraError::Ble("no BLE role could start".into()))
        }
    }

    async fn send(&self, link: LinkId, data: &[u8]) -> Result<()> {
        if link == PERIPHERAL_LINK {
            let mut server = self.server.lock().await;
            let server = server
                .as_mut()
                .ok_or_else(|| AmaraError::Ble("peripheral role not running".into()))?;
            return server
                .update_characteristic(Uuid::from_u128(TX_CHAR_UUID), data.to_vec())
                .await
                .map_err(|e| AmaraError::Ble(e.to_string()));
        }
        let (device, rx) = {
            let s = self.state.lock().unwrap();
            let l = s
                .links
                .get(&link)
                .ok_or_else(|| AmaraError::Ble("link down".into()))?;
            (l.device.clone(), l.rx.clone())
        };
        device
            .write(&rx, data, WriteType::WithoutResponse)
            .await
            .map_err(|e| AmaraError::Ble(e.to_string()))
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        let (links, subscribers) = {
            let s = self.state.lock().unwrap();
            (
                s.links.keys().copied().collect::<Vec<_>>(),
                !s.subscribers.is_empty(),
            )
        };
        for link in links {
            if let Err(e) = self.send(link, data).await {
                debug!("BLE write on link {link} failed: {e}");
            }
        }
        if subscribers {
            self.send(PERIPHERAL_LINK, data).await?;
        }
        Ok(())
    }

    async fn stop(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        if let Some(server) = self.server.lock().await.as_mut() {
            let _ = server.stop_advertising().await;
        }
        let devices: Vec<Peripheral> = self
            .state
            .lock()
            .unwrap()
            .links
            .drain()
            .map(|(_, l)| l.device)
            .collect();
        for d in devices {
            let _ = d.disconnect().await;
        }
    }

    fn max_write(&self) -> usize {
        self.config.write_size
    }
}
