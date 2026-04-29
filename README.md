# Sailfish 
[![rustc](https://img.shields.io/badge/rustc-1.51+-blue?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![license](https://img.shields.io/badge/license-Apache-blue.svg?style=flat-square)](LICENSE)

The code in this branch is a prototype of Sailfish, based on the Narwhal HotStuff (Hotstuff-over-Narwhal) prototype. 

TODO: Update Readme 
- Install instructions (Tmux, Rust, python3-decorator)

## License
This software is licensed as [Apache 2.0](LICENSE).


python3 scripts/sweep.py \
    --config scripts/configs/my-cluster.yaml \
    --rates 4000 8000 16000 32000 48000 64000 \
    --duration 30 --repeat 5

  python3 scripts/sweep.py \
    --config scripts/configs/my-cluster.yaml \
    --rates 60000 \
    --duration 30 \
    --repeats 5 \
    --output-dir scripts/logs/sweep_20260419_215746

    