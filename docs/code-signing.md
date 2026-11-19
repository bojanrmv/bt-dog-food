# Code signing

The `dgf` binaries are **not code-signed yet**. A code-signing certificate that Windows trusts is issued by a certificate authority, to a person or organization whose identity it has checked. This page says what vouches for a release meanwhile, and the policy a certificate will be used under.

## What vouches for a release today

- **A build attestation.** For every file in a release's `SHA256SUMS`, a statement signed through Sigstore that the release workflow of this repository built it from the version's tag. Anyone can check it: [checking a copy](security-software.md#checking-a-copy).
- **nuget.org's signature.** nuget.org signs every package it serves. The release workflow publishes there through trusted publishing: nuget.org gives that workflow a short-lived key on each run, and no key is stored.
- **The file itself.** `dgf.exe` and the .NET assemblies name their product, version, author and source.

None of this is an Authenticode signature, which is what Windows and antivirus software look for on a program file. A self-signed certificate would not help: nothing trusts it.

## The policy

**What is signed.** Only files this repository's release workflow builds from a tag: `dgf.exe` for Windows, and the .NET assemblies `dgf-dup-cs.dll`, `DgfWorker.dll`, `DgfMtp.dll` and `dgf-launcher.dll`. Files from others (Microsoft's compiler) keep their own signatures.

**Where.** In the release workflow, on GitHub's runners. The certificate's key stays with the signing service and never reaches a developer's machine or the repository.

**Who.**

| Role | Who | What they may do |
| --- | --- | --- |
| Author and committer | [Bojan Pavlovic](https://github.com/bojanrmv) | change the source |
| Reviewer | the same, until there are more maintainers | review changes from others before they are merged |
| Approver | the same | approve a release: the publish step of the workflow waits for it |

Changes from anyone else come as pull requests and are reviewed before they are merged.

**What a release has to pass.** The tests and lints of [CI](../.github/workflows/ci.yml), the release workflow's own install-and-run test of the packed tool on Windows, Linux and macOS, and the approval.

**Privacy.** `dgf` does not transfer any information to other networked systems: it has no network code. The tools it starts on the user's request (`dotnet`, `node`, `git`) follow their own settings.

## Getting a certificate

| Option | Cost | Notes |
| --- | --- | --- |
| [SignPath Foundation](https://signpath.org) | free for open-source projects | by application, under its terms: a public project with an open-source license and a published code-signing policy, which the one above is written to become |
| Microsoft's Trusted Signing service (Azure) | a monthly fee | identity validation of a person or an organization; signs from GitHub Actions |
| A certificate from a certificate authority | a yearly fee | the key is on a hardware token or in a cloud key store |

When there is one, the signing step goes in the release workflow before the checksums are made, since signing changes a file's hash: `dgf.exe` in the `binaries` job for the Windows targets, the .NET assemblies in the `pack` job after they are published. The attestation then covers the signed files. A new certificate has no reputation yet: for the first weeks a signed file can still be flagged, though by fewer products.
