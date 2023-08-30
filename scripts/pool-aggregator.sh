#!/bin/bash
source $(dirname $0)/executor.sh
if [[ "$NETWORK" == "" ]]; then
    NETWORK=mainnet
fi

APP=pool-aggregator execute $@
