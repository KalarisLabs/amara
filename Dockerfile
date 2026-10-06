FROM rust:1.80-slim-bullseye AS builder

WORKDIR /usr/src/amara

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire workspace
COPY . .

# Build the CLI release binary
RUN cargo build --release --bin amara-cli

# Create a lightweight runtime image
FROM debian:bullseye-slim

WORKDIR /app

# Copy the compiled binary from the builder
COPY --from=builder /usr/src/amara/target/release/amara-cli /usr/local/bin/amara-cli

# Set the entrypoint to the CLI
ENTRYPOINT ["amara-cli"]

# Default command launches the TUI
CMD ["tui"]
