# syntax=docker/dockerfile:1.7-labs
### STAGE 0: Create base chef image for building
### cargo chef is used to speed up the build process by caching dependencies using docker
FROM --platform=$TARGETPLATFORM lukemathwalker/cargo-chef:latest-rust-latest as chef

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
RUN apt-get update && apt-get -y upgrade && apt-get install -y gcc libclang-dev pkg-config libssl-dev

RUN --mount=type=ssh cargo chef cook --release --recipe-path recipe.json --bin transaction-submitter 
COPY --exclude=target . .

RUN --mount=type=ssh cargo build --release --bin transaction-submitter

# Stage 3: Final image for running in the env
FROM --platform=$TARGETPLATFORM debian:bookworm-slim
RUN apt-get update && apt-get -y upgrade && apt-get install -y libssl-dev ca-certificates 

COPY --from=builder /app/target/release/transaction-submitter /usr/local/bin/transaction-submitter

ENTRYPOINT [ "/usr/local/bin/transaction-submitter" ]
