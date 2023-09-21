# TEE-builder

[![License](https://img.shields.io/badge/license-Apache2-green.svg)](LICENSE)

**WARNING: This is not for production use. There are debug and info logs that leaks information within the enclave.**

## About

In today's advancement of blockchain technology, the security of Block Builders has gained unprecedented attention, particularly against the backdrop of PBS (Proposer-builder separation). Should a Block Builder be compromised, the potential economic loss from the leakage of mev value could be substantial. Traditional Block Builders, typically implemented based on geth, are inherently bulky and have high hardware demands, making them challenging to fit within a TEE.

To address these challenges, we introduced the "tee stateless block builder" design. Not only does it drastically reduce startup time, but it also offers flexible memory management and diminishes the dependency on Geth. Most importantly, our performance optimizations allow this design to remain lightweight while delivering speeds close to local execution.


## Architecture

![architecture](docs/architecture.png)

This system's architecture is comprised of two main components: mempool and builder. They are decoupled and can operate as independent services, allowing the mempool to serve multiple builders, and likewise, builders to connect to multiple mempools.

These components all run within the protected environment of SGX and utilize remote attestation technology to verify if they are operating within an enclave, thereby ensuring the security of communication. Upon initial communication, they will exchange internally generated random keys, and all subsequent communication data will be transmitted through symmetric encryption.

Within this architectural framework, the mempool is responsible for receiving users' private transactions or bundles, and pushing them to the builder for processing. The builder, on the other hand, collects transactions or bundles from various channels such as mempool and geth public mempool, achieving centralized processing and collaborative work of transactions.


## See also

The project extensively utilizes SGX Libraries:
* [sgxlib](https://github.com/automata-network/sgxlib)
* [sgxlib-thirdparty](https://github.com/automata-network/sgxlib-thirdparty)
* [statedb-rs](https://github.com/automata-network/statedb-rs)
* [eth-types-rs](https://github.com/automata-network/eth-types-rs)
* [evm-rs](https://github.com/automata-network/evm-rs)

## Contributing

**Before You Contribute**:
* **Raise an Issue**: If you find a bug or wish to suggest a feature, please open an issue first to discuss it. Detail the bug or feature so we understand your intention.  
* **Pull Requests (PR)**: Before submitting a PR, ensure:  
    * Your contribution successfully builds.
    * It's linted using `cargo fmt`.
    * It includes tests, if applicable.

## License

Apache2