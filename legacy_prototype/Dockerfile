# ---- Builder Stage ----
# Use the official Rust image as a builder
FROM rust:1.78 as builder

# Create a new empty shell project
WORKDIR /usr/src/synapse
COPY . .

# Build for release
RUN cargo build --release

# ---- Final Stage ----
# Use a minimal, secure base image
FROM debian:bullseye-slim

# Copy the compiled binary from the builder stage
COPY --from=builder /usr/src/synapse/target/release/synapse /usr/local/bin/synapse

# Copy the .env file
# Note: In a real production setup, secrets should be handled via environment variables
# passed by the orchestrator (e.g., Docker Compose, Kubernetes), not copied in the image.
COPY .env .

# Set the binary as the entrypoint
CMD ["synapse"]
