# Dependency maintenance

Dependabot checks Cargo, container images and GitHub Actions weekly. Version
updates are grouped into one PR per ecosystem, with at most three such PRs
open. Security updates, when enabled in the repository settings, are separate
from these groups and limits. This configuration does not enable or disable
that repository setting. Merged PR branches are deleted automatically.

Review the actual changes and require passing Rust, web, CAD integration and
container smoke checks before merging. A passing Linux CI run does not prove
that native installers or authenticated image publishing work; review changes
to those workflows against their upstream action documentation as well.

## Deferred API migrations

The following version-only PRs were closed on 2026-09-18 because they fail the
existing compatibility checks. Closing them does not apply their changes.

| Dependencies | Closed PRs | Required work |
| --- | --- | --- |
| `eframe`, `egui`, `egui_glow`, `glow` | [#23](https://github.com/tomeido/mantis-cad/pull/23), [#26](https://github.com/tomeido/mantis-cad/pull/26) | Upgrade the GUI stack together, migrate the `App` callbacks, and reconcile renderer types. |
| `argon2` | [#25](https://github.com/tomeido/mantis-cad/pull/25) | Configure the new `getrandom` dependency for browsers and validate native/web identity-backup compatibility. |
| `quick-xml` | [#27](https://github.com/tomeido/mantis-cad/pull/27) | Migrate the GHX parser to the new string and decoding APIs, then run the native Grasshopper fixture tests. |
| `sha2`, `ed25519-dalek` | [#31](https://github.com/tomeido/mantis-cad/pull/31) | Coordinate the cryptographic trait and RNG changes, then validate signatures, signed-history replay and native/web builds. |

Routine Cargo PRs exclude major **version updates** so they do not mix API
migrations with compatible updates. Cargo treats changes to the minor line of
a `0.x` dependency as breaking major updates. Compatible patches and minor
updates remain eligible. The rule intentionally omits `versions` ranges:
Dependabot bypasses `ignore.update-types` for security updates.

For an intentional major upgrade, update the manifests and code in a dedicated
migration PR. Once merged, Dependabot follows the new compatible release line.
Do not merge isolated GUI library updates that create incompatible renderer types.

References: [GitHub configuration options](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference),
[Cargo compatibility rules](https://github.com/dependabot/dependabot-core/blob/69605c903ab2db2b3f2ea302eda30dcec1d0ddc1/cargo/lib/dependabot/cargo/version.rb),
[security-update handling](https://github.com/dependabot/dependabot-core/blob/69605c903ab2db2b3f2ea302eda30dcec1d0ddc1/common/lib/dependabot/config/ignore_condition.rb).

## Download and build artifacts

Keep published release assets and tags. CI bundles and caches for closed PRs
can be removed after recording the PR and artifact metadata. Retain current
main-branch outputs and release packages that have not been published yet.
Container build records expire after 14 days; the native release workflow
also retains its downloadable packages for 14 days, so archive needed packages
before expiry if publication fails.
