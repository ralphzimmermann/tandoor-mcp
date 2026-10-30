# Multi-stage build for mcp-tandoor server
#
# Build stage. The default pulls the official image from Docker Hub, which is
# what GitHub-hosted CI and local builds should use: Public ECR throttles
# anonymous pulls per source IP, and shared runner IPs hit its 429 limit.
# Deployment hosts that see Docker Hub failures override RUST_IMAGE with the
# Public ECR mirror (see docker-compose.yml).
ARG RUST_IMAGE=rust:1.89-slim-bookworm
FROM ${RUST_IMAGE} AS builder

# Install system dependencies for building
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Build dependencies against stub sources so this layer stays cached until
# Cargo.toml changes, instead of recompiling every crate on each source edit
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# Copy source code
COPY src ./src

# Build the application in release mode (touch so cargo sees the real sources as newer)
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage using distroless
FROM gcr.io/distroless/cc-debian12:latest

# Create a non-root user for security
USER 1000:1000

# Copy the binary from builder stage
COPY --from=builder /app/target/release/mcp-tandoor /usr/local/bin/mcp-tandoor

# Set environment variables with defaults
ENV TANDOOR_BASE_URL=http://localhost:8080
ENV BIND_ADDR=0.0.0.0:3001
ENV RUST_LOG=info

# Expose the port
EXPOSE 3001

# Run the binary
ENTRYPOINT ["/usr/local/bin/mcp-tandoor"]
