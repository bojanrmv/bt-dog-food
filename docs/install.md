# Install

## As a .NET tool

With the [.NET 10 SDK](https://dotnet.microsoft.com/download) or newer:

```powershell
dotnet tool install -g dgf
dgf --version
```

This is the native `dgf` binary for your platform (Windows, Linux or macOS, x64 or arm64), with the C# duplication engine beside it: `dgf dup --semantic` works with nothing else installed. The command runs the binary directly, so it is as fast as one built from source.

```powershell
dotnet tool update -g dgf      # update
dotnet tool uninstall -g dgf   # remove
```

For one repository only, pin the version in its tool manifest so CI and everyone on the team run the same one:

```powershell
dotnet new tool-manifest       # once, creates .config/dotnet-tools.json
dotnet tool install dgf
dotnet tool run dgf analyze    # or: dotnet dgf analyze
```

Each [GitHub release](https://github.com/bojanrmv/bt-dog-food/releases) also has the binary and the engine as a download per platform, for machines without .NET.

## From source

Building takes a few minutes, once.

### 1. Tools, once per machine

| System | Rust | C compiler (the language grammars are C) |
| --- | --- | --- |
| Windows | `winget install Rustlang.Rustup` | `winget install Microsoft.VisualStudio.2022.BuildTools`, then pick "Desktop development with C++" |
| macOS | [rustup.rs](https://rustup.rs) | `xcode-select --install` |
| Linux | [rustup.rs](https://rustup.rs) | `build-essential` (Debian, Ubuntu) or `gcc` |

### 2. Install `dgf`

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
| `dgf dup --semantic` (C#), when built from source | the .NET 10 SDK, then `dotnet tool install -g dgf-dup-cs`: see [the C# engine](duplication/csharp.md) |
| `dgf mutate --run` on C# | the .NET SDK (`dotnet`) |
| `dgf mutate --run` on TypeScript and JavaScript | Node.js 22.3 or newer, with the project's `node_modules` |

Everything else needs nothing but the `dgf` binary.

## Update and remove a source build

```powershell
cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-cli --locked --force   # update
cargo uninstall dog-food-cli                                                                # remove
```

Next: [set it up in a repository](setup-in-another-repo.md).
