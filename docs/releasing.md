# Releasing

A release is a version tag. The [release workflow](../.github/workflows/release.yml) builds `dgf` for every platform, packs it as the `dgf` .NET tool, installs and runs that package on Windows, Linux and macOS, and, once approved, publishes it to nuget.org and creates the GitHub release.

## Versions

One version for everything: the Rust crates (`version` in the root `Cargo.toml`), the NuGet packages and the tag. The workflow refuses a tag that does not match `Cargo.toml`.

Versions follow [semantic versioning](https://semver.org). Until 1.0:

| Bump | When | Examples |
| --- | --- | --- |
| minor, `0.2.0` → `0.3.0` | something users rely on changes | a flag renamed or removed, a `dgf.toml` key changed, a field gone from the JSON, SARIF or agent output, an exit code that means something else |
| patch, `0.2.0` → `0.2.1` | fixes and additions that change nothing users rely on | a wrong score, a crash, a new flag, a new mutator |

1.0 comes when the command line, `dgf.toml` and the JSON output are settled; after it, a breaking change is a major version.

A prerelease (`0.3.0-rc.1`) goes to nuget.org as a prerelease: nobody gets it unless they ask (`dotnet tool update -g dgf --prerelease`). Use one to try a larger change on a real team before everyone gets it.

There is no fixed schedule: release when something users want has landed. Fixes for reported problems go out as patches soon after they land.

## Making a release

```powershell
# 1. The version, in the root Cargo.toml ([workspace.package] version)
cargo check                                  # updates Cargo.lock to match
git commit -am "release 0.2.0"
git push

# 2. The tag starts the release
git tag v0.2.0
git push origin v0.2.0
```

3. In GitHub Actions, the `release` run builds, packs and tests (about 15 minutes), then waits at **publish**. Approve it (Review deployments → `nuget`).
4. It pushes to nuget.org and creates the GitHub release with notes made from the commits since the last tag. Edit the notes there if they need it.
5. Check it as a user would. nuget.org takes a few minutes to index a new version:

```powershell
dotnet tool update -g dgf
dgf --version
```

To try the build without publishing: Actions → release → **Run workflow**. It does everything but the publish step, and the packages are kept as the run's artifacts.

To try the package on this machine before a release:

```powershell
./scripts/pack-nuget.ps1
dotnet tool install -g dgf --add-source packaging/nuget/nupkg --version 0.2.0
```

## What a release publishes

| Where | What |
| --- | --- |
| nuget.org `dgf` | the .NET tool: `dotnet tool install -g dgf`. A small package that points to one package per platform (`dgf.win-x64`, `dgf.linux-arm64`, …), so an install downloads only its own. |
| nuget.org `dgf.<platform>` | the native `dgf` binary for that platform, and the C# engine in `engine/` |
| nuget.org `dgf-dup-cs` | the C# engine alone, for those who install `dgf` with cargo |
| GitHub release | per platform, an archive with `dgf`, the C# engine in `engine/`, the README and the license |

Platforms: Windows, Linux and macOS, each on x64 and arm64. The Linux binaries are static, so they run on any distribution.

The package does not wrap `dgf` in .NET: the tool's command is the native binary itself, so it starts and runs as fast as one built with cargo. It needs the .NET 10 runtime only for the C# engine (`dgf dup --semantic`) and for mutation runs on C#. `dgf` finds the engine in `engine/` beside itself.

## When a release is wrong

A version on nuget.org cannot be deleted or replaced, only unlisted. Unlisting hides it from search and from `dotnet tool install` without a version, but anyone who asks for it by number still gets it.

1. Unlist the bad version on nuget.org: each of the `dgf` packages, under Manage package → Listing.
2. Fix it and release the next patch.
3. Mark the GitHub release as a prerelease, or delete it.

## Set up once

What publishing needs, once per repository:

1. **nuget.org trusted publishing.** On nuget.org, under your user → Trusted Publishing, a policy for this repository (owner and name), workflow `release.yml`, environment `nuget`. The workflow gets a short-lived key on each run; no key is stored.
2. **The `NUGET_USER` secret.** In the repository's Actions secrets, the nuget.org username (the profile name, not the email address).
3. **The `nuget` environment.** In the repository's Environments, with yourself as a required reviewer, so nothing is published without approval.
