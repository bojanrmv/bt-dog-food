# Security

## Reporting a vulnerability

Report a weakness in `dgf` privately: on the repository's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/bojanrmv/bt-dog-food/security/advisories/new)). Say which version, what happens, and how to see it happen. If that page is not open to you, [open an issue](https://github.com/bojanrmv/bt-dog-food/issues) that only asks for a private channel, without the details.

dog-food is maintained in spare time, on a best-effort basis. A report that holds is fixed in the next release, and fixes go into the latest version only: `dotnet tool update -g dgf`.

What counts: `dgf` writing or running something outside what its command is meant to, a crafted source file, `dgf.toml` or coverage report that makes it do so, a dependency with a known vulnerability, or anything wrong with how releases are built and published.

What does not: `dgf mutate --run` builds a repository and runs its tests, so a repository you do not trust can run code that way, as it can with `dotnet test` or `npm test`.

## Antivirus software flags dgf

That is a false positive, not a vulnerability, and it has its own page: [for administrators and security teams](docs/security-software.md) says what `dgf` does on a machine, how to check that a copy is the real one, and how to report the detection.

## Checking a release

Every release is built from its tag by the repository's [release workflow](.github/workflows/release.yml), never on a developer's machine, and has `SHA256SUMS` and a signed build attestation:

```powershell
gh attestation verify <path to dgf> --repo bojanrmv/bt-dog-food
```

The binaries are not code-signed yet: [code signing](docs/code-signing.md).
