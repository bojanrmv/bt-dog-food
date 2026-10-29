# Install

There are no prebuilt downloads yet, so `dgf` is built from source. It takes a few minutes, once.

## 1. Tools, once per machine

| System | Rust | C compiler (the language grammars are C) |
| --- | --- | --- |
| Windows | `winget install Rustlang.Rustup` | `winget install Microsoft.VisualStudio.2022.BuildTools`, then pick "Desktop development with C++" |
| macOS | [rustup.rs](https://rustup.rs) | `xcode-select --install` |
| Linux | [rustup.rs](https://rustup.rs) | `build-essential` (Debian, Ubuntu) or `gcc` |

## 2. Install `dgf`

```powershell
cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-cli --locked
dgf --version
```

`dog-food-cli` is the package; the command it installs is `dgf`. `--locked` uses the exact library versions the repository was tested with. Add `--rev <commit>` for one version.

From a clone, to change the code or work offline:

```powershell
git clone https://github.com/bojanrmv/bt-dog-food
cd bt-dog-food
cargo install --path crates/dog-food-cli --locked
```

`dgf` lands in `~/.cargo/bin`, which the Rust installer puts on the PATH. If a terminal says `dgf` is not recognized, it was opened before Rust was installed: open a new one, or on Windows run `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`.

## Optional parts

| For | Install |
| --- | --- |
| `dgf-dup` on its own (the same as `dgf dup`) | `cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-dup --locked` |
| `dgf dup --semantic` (C#) | the .NET 10 SDK, then the `dgf-dup-cs` tool: see [the C# engine](duplication/csharp.md) |
| `dgf mutate --run` on C# | the .NET SDK (`dotnet`) |
| `dgf mutate --run` on TypeScript and JavaScript | Node.js 22.3 or newer, with the project's `node_modules` |

Everything else needs nothing but the `dgf` binary.

## Update and remove

```powershell
cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-cli --locked --force   # update
cargo uninstall dog-food-cli                                                                # remove
```

Next: [set it up in a repository](setup-in-another-repo.md).
