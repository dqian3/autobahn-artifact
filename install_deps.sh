sudo apt update
sudo apt install clang tmux
curl https://sh.rustup.rs -sSf | sh -s -- -y


sudo apt remove python3-pip
wget https://bootstrap.pypa.io/get-pip.py
python3 get-pip.py

export PATH=$PATH:~/.local/bin
source $HOME/.cargo/env