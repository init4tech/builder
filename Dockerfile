# syntax=docker/dockerfile:1.7-labs
### STAGE 0: Create base chef image for building
### Use a Debian bookworm-based Rust image so GLIBC matches the final runtime (bookworm ships glibc 2.36)
### cargo-chef is then installed into this pinned base
FROM --platform=$TARGETPLATFORM rust:1.90-bookworm AS chef

RUN cargo install cargo-chef

WORKDIR /app

### Stage 1: cargo chef prepare
### Creates the recipe.json file which is a manifest of Cargo.toml files and 
### the relevant Cargo.lock file
FROM chef as planner
COPY --exclude=target . .
RUN cargo chef prepare

### Stage 2: Build the project
### This stage builds the deps of the project (not the code) using cargo chef cook
### and then it copies the source code and builds the actual crates
### this takes advantage of docker layer caching to the max
FROM chef as builder
COPY --from=planner /app/recipe.json recipe.json
RUN apt-get update && apt-get -y upgrade && apt-get install -y \
	gcc \
	libclang-dev \
	pkg-config \
	libssl-dev
	# git \
	# openssh-client \
	# make \
	# perl

RUN --mount=type=ssh cargo chef cook --release --recipe-path recipe.json --bin zenith-builder-example 
COPY --exclude=target . .

RUN --mount=type=ssh cargo build --release --bin zenith-builder-example

# Stage 3: Final image for running in the env
FROM --platform=$TARGETPLATFORM debian:bookworm-slim
RUN apt-get update && apt-get -y upgrade && apt-get install -y \
		libssl3 \
		ca-certificates \
	&& rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/zenith-builder-example /usr/local/bin/zenith-builder-example

ENTRYPOINT [ "/usr/local/bin/zenith-builder-example" ]
