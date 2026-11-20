# Security

## Reporting a vulnerability

Report it privately: on the repository's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/bojanrmv/bt-dog-food/security/advisories/new)). Fixes go into the latest version.

## What dgf does on a machine

- It makes no network connections, asks for no administrator rights and installs nothing.
- It starts `git`, `dotnet` and `node`, and no other program.
- `dgf mutate --run` copies the projects under test to a sandbox folder, builds them and runs their tests there. That folder is `%TEMP%/dgf-mutate`, or the one [`sandbox_dir`](docs/mutation/settings.md) names.

## Checking a copy

Every release is built from its tag by the [release workflow](.github/workflows/release.yml), and has a `SHA256SUMS` file and a signed build attestation:

```powershell
gh attestation verify <path to dgf> --repo bojanrmv/bt-dog-food
```

## Antivirus software

The binaries are not code-signed, so antivirus software may distrust them. If it stops a copy that passes the check above, that is a false positive: allow the `dgf` binary and the sandbox folder, and report it to the vendor (Microsoft: [microsoft.com/wdsi/filesubmission](https://www.microsoft.com/wdsi/filesubmission)).
