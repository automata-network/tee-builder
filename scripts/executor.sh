#!/bin/bash

function execute() {
    cd $(dirname $0)/..
    TARGET_ENV="std"
    if [[ "$SGX" != "" ]]; then
        TARGET_ENV="sgx"
    fi

    PKG=$APP
    if [[ "$TARGET_ENV" == "sgx" ]]; then
        PKG="automata-$PKG"
        if [[ "$INC" == "" ]]; then
            rm -rf bin/sgx/target/*/build/$PKG-*
        fi 
    fi
    dir="bin/$TARGET_ENV/$PKG"

    profile=""
    if [[ "$RELEASE" != "" ]]; then
        profile=" --release "
    fi

    if [[ "$NETWORK" != "" ]]; then
        CONFIG="-c config/$APP-$NETWORK.json"
    fi

    if [[ "$RUST_LOG" == "" ]]; then
        RUST_LOG="info"
    fi 

    build_arg=" --manifest-path=$dir/Cargo.toml"

    if [[ "$BUILD" != "" ]]; then
        cargo build $build_arg $profile
    elif [[ "$FETCH" != "" ]]; then
        cargo fetch $build_arg
    else
        RUST_BACKTRACE=1 cargo run $profile $build_arg -- $CONFIG $@
    fi
}