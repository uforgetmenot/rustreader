#!/bin/bash

source "$HOME/.cargo/env"
cargo tauri build --no-bundle
