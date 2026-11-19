# For administrators and security teams

What `dgf` is, what it does on a machine, how to check that a copy is the real one, and what to do when antivirus software stops it.

## In short

| | |
| --- | --- |
| What | A command-line tool for developers: code complexity, duplicate code and mutation testing for C#, TypeScript, JavaScript and Rust |
| Author | Bojan Pavlovic, [github.com/bojanrmv](https://github.com/bojanrmv) |
| Source | [github.com/bojanrmv/bt-dog-food](https://github.com/bojanrmv/bt-dog-food), MIT license; every release is built from a tag of that repository |
| Comes from | nuget.org (`dgf`, `dgf.<platform>`, `dgf-dup-cs`) and the repository's GitHub releases |
| Built by | the repository's [release workflow](../.github/workflows/release.yml) on GitHub's runners; nothing is published from a developer's machine |
| Network | none: `dgf` has no network code |
| Rights | those of the user who runs it; it never asks for more |
| Code signing | not yet: see [code signing](code-signing.md). Each release has checksums and a signed build attestation instead |

## What a package contains

The package for Windows on x64, as an example. `./scripts/check-package.ps1 -Version <version>` lists these files with their product, author, version and SHA-256, and scans them with Microsoft Defender.

| File | What it is |
| --- | --- |
| `dgf.exe` | the tool: a native program written in Rust. Its properties name the product, version, author and source |
| `engine/dgf-dup-cs.dll` | the C# engine of `dgf dup`, a .NET program that reads C# with Microsoft's compiler |
| `engine/Microsoft.CodeAnalysis*.dll` | Microsoft's C# compiler (Roslyn), signed by Microsoft |
| `engine/worker/DgfWorker.dll`, `engine/mtp/DgfMtp.dll` | the test workers of `dgf mutate --run`: .NET programs that load a test project and run its tests |
| `dgf-launcher.dll` | a placeholder a .NET tool package has to contain; it is never run |

## What each command does

| Command | Reads | Writes | Starts |
| --- | --- | --- | --- |
| `dgf analyze`, `dgf changes`, `dgf history` | source files, `dgf.toml`, coverage reports | the report (the terminal, or the file `-o` names); `.dgf/history` with `--save-history` | `git` |
| `dgf baseline`, `dgf init` | source files | `.dgf/baseline.json`, `dgf.toml` | `git` |
| `dgf dup` | source files | the report | `git`; on C#, `dotnet` with the engine beside the binary |
| `dgf dup --confirm` | the same | the same | the same; the engine also compiles the methods it compares in memory and runs them on generated inputs, in its own process |
| `dgf mutate` (without `--run`) | source files | the report | `git` |
| `dgf mutate --run` | the repository | the sandbox folder, `.dgf/mutation-cache` | `git`, `dotnet`, `node`: builds and test runs, in the sandbox |
| `dgf hook install`, `dgf skill install` | | the one file each names: `.git/hooks/pre-commit`, `.claude/skills/dog-food/SKILL.md` | `git` |

`dgf mutate --run` is the command security software notices, because it does what a build server does. It copies the projects under test to a sandbox folder, changes the copies, builds them, runs their tests there, and stops test processes that hang. The working tree is not changed. The tests are the repository's own, so they do there whatever they do when a developer runs them.

- **One folder.** Everything it builds and runs is in `%TEMP%/dgf-mutate`, or in the folder [`sandbox_dir`](mutation/settings.md) or the `DGF_SANDBOX_DIR` variable names. That folder and the `dgf` binary are all there is to allow.
- **Processes.** It starts `git`, `dotnet` and `node`, and no other program: no shell, no script host, no system tool. It stops only the builds and test processes of its own run, and it does not look at other processes on the machine.
- **Network.** None by `dgf`. The tools it starts may use it: `dotnet` downloads packages when it restores a project. `dgf` turns the .NET command line's telemetry off for the builds it starts.
- **The system.** No registry keys, no services, no scheduled tasks, nothing that runs at startup, nothing installed. It asks for no administrator rights and its manifest says so.

## Checking a copy

Releases made after 0.1.4 carry what this needs.

**Its hash.** Every GitHub release has `SHA256SUMS`, with the SHA-256 of each download and of each `dgf` binary by itself. The binary a .NET tool install puts on disk is that same file:

```powershell
$dgf = Get-ChildItem "$env:USERPROFILE\.dotnet\tools\.store\dgf" -Recurse -Filter dgf.exe | Select-Object -Last 1
(Get-FileHash $dgf.FullName -Algorithm SHA256).Hash
```

**Where it was built.** Each release has a build provenance attestation: a statement signed through [Sigstore](https://www.sigstore.dev), with a certificate issued to the release workflow of this repository and recorded in a public log, that the files in `SHA256SUMS` were built by that workflow from that version's tag. The GitHub CLI checks it:

```powershell
gh attestation verify $dgf.FullName --repo bojanrmv/bt-dog-food
```

The statement is also a download of the release, for a check that does not ask GitHub for it: add `--bundle dgf-<version>.attestation.jsonl`.

**What it says it is.** The file's properties (Details) name the product, the version, the author and the source.

**What it is made of.** The source of a version is its tag. `Cargo.lock` names every library in the binary with a checksum, the build uses exactly those (`--locked`), and they are checked against the [RustSec advisories](https://rustsec.org) every week.

## When antivirus software stops it

Nothing in `dgf` is harmful, and a copy that passes the check above is ours. A detection of such a copy is a false positive, of one of two kinds:

- **The file.** A new, unsigned program that few machines have seen is what antivirus software trusts least. A detection whose name ends in `!ml` (Microsoft's `Wacatac.B!ml`, for one) is a classifier's guess about such a file, not something found in it. Versions up to 0.1.4 also had no product, version or author in the file.
- **What it does.** A name that starts with `Behavior:`, or a detection of a file in the sandbox folder, comes from `dgf mutate --run` building and running code. Versions up to 0.1.4 also started `taskkill` and `cmd`, which later versions do not.

What to do:

1. Note the detection name, the path of the file it names, the `dgf` version and the SHA-256 of the binary.
2. Check the copy as above. A file that fails the check is not ours: do not allow it.
3. Allow the `dgf` binary and one sandbox folder. Set the folder with `DGF_SANDBOX_DIR`, so it is the same on every run.
4. Report the false positive to the antivirus vendor, so the next user is not stopped. Microsoft takes files from developers and from customers at [microsoft.com/wdsi/filesubmission](https://www.microsoft.com/wdsi/filesubmission); other vendors have a form of the same kind. [VirusTotal](https://www.virustotal.com) shows which engines flag a file.
5. Tell us: [open an issue](https://github.com/bojanrmv/bt-dog-food/issues) with the detection name, the version and the antivirus product, so the cause can be removed where there is one.

A weakness in `dgf` itself is reported differently: see [SECURITY.md](../SECURITY.md).
