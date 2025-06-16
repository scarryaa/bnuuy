# bnuuy 🐰

A quirky (and simple, fast, and GPU-accelerated) terminal emulator written in Rust.

## Building and Running

You will need the Rust toolchain installed.

1.  **Clone the repository:**
    ```bash
    git clone https://github.com/scarryaa/bnuuy.git
    cd bnuuy
    ```

2.  **Run the application:**
    ```bash
    cargo run --release
    ```

## Configuration

`bnuuy` looks for a configuration file at the following locations:

| Operating System | Configuration File Path                                      |
| ---------------- | ------------------------------------------------------------ |
| **Linux**        | `~/.config/bnuuy/config.toml`                                |
| **macOS**        | `~/Library/Application Support/lt.scar.bnuuy/config.toml`    |
| **Windows**      | `%APPDATA%\scar\bnuuy\config\config.toml` <br> (e.g., `C:\Users\User\AppData\Roaming\scar\bnuuy\config\config.toml`) |


If the file doesn't exist, it will use default values. You can create a `config.toml` file to override them.

**Example `config.toml`:**

```toml
# Font size in pixels
font_size = 15.0

# The shell command and its arguments to launch
# On Windows, you might use: shell = ["powershell.exe"]
shell = ["bash", "-i"]

[colors]
# Colors are defined as (red, green, blue) tuples from 0-255
foreground = [192, 192, 192]
background = [0, 0, 0]
```
