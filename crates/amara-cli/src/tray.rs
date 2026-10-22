//! `amara tray`: the desktop app. A node with the HTTP API runs in the
//! background; the menu-bar/system-tray icon shows who is nearby and opens the
//! dashboard (`/ui`) in the browser.

use crate::config::NodeArgs;
use crate::{dashboard_link, serve_daemon, start_daemon};
use amara_agent::Agent;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Default)]
struct Shared {
    agent: Option<Arc<Agent>>,
    link: Option<String>,
    error: Option<String>,
}

enum UserEvent {
    Menu(MenuEvent),
}

/// A 32×32 icon drawn in code: three linked nodes on the brand purple.
fn icon() -> Icon {
    const N: u32 = 32;
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    let nodes = [(10.0f32, 11.0f32), (22.0, 11.0), (16.0, 22.0)];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let i = ((y * N + x) * 4) as usize;
            let in_disc = (fx - 16.0).powi(2) + (fy - 16.0).powi(2) <= 15.5f32.powi(2);
            if !in_disc {
                continue;
            }
            let on_node = nodes
                .iter()
                .any(|(nx, ny)| (fx - nx).powi(2) + (fy - ny).powi(2) <= 10.0);
            let on_edge = nodes
                .iter()
                .zip(nodes.iter().cycle().skip(1))
                .any(|(a, b)| {
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let t =
                        (((fx - a.0) * dx + (fy - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                    (fx - a.0 - t * dx).powi(2) + (fy - a.1 - t * dy).powi(2) <= 1.2
                });
            let px = if on_node || on_edge {
                [255, 255, 255, 255]
            } else {
                [0x7c, 0x3a, 0xed, 255]
            };
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon")
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        eprintln!("cannot open browser ({e}); visit {url}");
    }
}

pub fn run(args: NodeArgs) -> Result<()> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    // The node and API live on their own Tokio runtime; the UI owns the main thread.
    let node_shared = shared.clone();
    let node = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async move {
            let daemon = match start_daemon(&args, None).await {
                Ok(d) => d,
                Err(e) => {
                    node_shared.lock().unwrap().error = Some(format!("{e:#}"));
                    return;
                }
            };
            {
                let mut s = node_shared.lock().unwrap();
                s.agent = Some(daemon.agent.clone());
                let base = daemon.url.replace("0.0.0.0", "127.0.0.1");
                s.link = Some(dashboard_link(&base, &daemon.token));
            }
            let shutdown = async {
                let _ = stop_rx.await;
            };
            if let Err(e) = serve_daemon(daemon, shutdown).await {
                node_shared.lock().unwrap().error = Some(format!("{e:#}"));
            }
        });
    });

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let title = MenuItem::new("AMARA · starting…", false, None);
    let peers = MenuItem::new("Looking for agents…", false, None);
    let held = MenuItem::new("", false, None);
    let open = MenuItem::new("Open dashboard", true, None);
    let quit = MenuItem::new("Quit AMARA", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &title,
        &peers,
        &held,
        &PredefinedMenuItem::separator(),
        &open,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .context("building tray menu")?;

    let mut tray: Option<TrayIcon> = None;
    let mut stop_tx = Some(stop_tx);
    let mut node = Some(node);
    let mut last_peers = usize::MAX;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(2));
        match event {
            // Tray icons must be created once the loop is running (macOS).
            Event::NewEvents(StartCause::Init) => {
                tray = TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_tooltip("AMARA mesh")
                    .with_icon(icon())
                    .build()
                    .map_err(|e| eprintln!("cannot create tray icon: {e}"))
                    .ok();
            }
            Event::UserEvent(UserEvent::Menu(e)) if e.id == open.id() => {
                match shared.lock().unwrap().link.clone() {
                    Some(link) => open_in_browser(&link),
                    None => eprintln!("the node is still starting"),
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) if e.id == quit.id() => {
                if let Some(tx) = stop_tx.take() {
                    let _ = tx.send(());
                }
                if let Some(node) = node.take() {
                    let _ = node.join();
                }
                tray.take();
                *control_flow = ControlFlow::Exit;
            }
            Event::NewEvents(_) => {
                let s = shared.lock().unwrap();
                if let Some(err) = &s.error {
                    title.set_text("AMARA · error");
                    peers.set_text(err.chars().take(80).collect::<String>());
                } else if let Some(agent) = &s.agent {
                    let id = agent.identity();
                    title.set_text(format!(
                        "AMARA · {} ({})",
                        id.display_name(),
                        id.peer_id().short_hex()
                    ));
                    let list = agent.peers();
                    if list.len() != last_peers {
                        last_peers = list.len();
                        let names: Vec<String> =
                            list.iter().take(6).map(|p| p.name.clone()).collect();
                        peers.set_text(match list.len() {
                            0 => "No agents nearby yet".to_string(),
                            n => format!(
                                "{n} nearby: {}{}",
                                names.join(", "),
                                if n > 6 { ", …" } else { "" }
                            ),
                        });
                        if let Some(t) = &tray {
                            let _ = t.set_tooltip(Some(format!(
                                "AMARA · {} agent(s) nearby",
                                list.len()
                            )));
                        }
                    }
                    let waiting: usize = agent.mesh().held().iter().map(|(_, n)| n).sum();
                    held.set_text(if waiting > 0 {
                        format!("{waiting} message(s) held for offline agents")
                    } else {
                        String::new()
                    });
                }
            }
            _ => {}
        }
    });
}
