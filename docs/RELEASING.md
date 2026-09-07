# Releasing Ceiling

Ceiling releases are built from a clean, protected `main` branch and published
from an immutable version tag. Do not build public artifacts from a dirty
working tree.

## Prepare

1. Choose the next version and update `version.env`, both Rust manifests,
   `apps/desktop-tauri/package.json`, and `tauri.conf.json`.
2. Move the relevant entries from `Unreleased` into a dated changelog section.
3. Run the complete local validation:

   ```powershell
   powershell.exe -ExecutionPolicy Bypass -NoProfile -File scripts\local-check.ps1 -All -Version <version>
   ```

4. Open a pull request and wait for the hosted `Frontend` and `Rust` checks.
5. Merge with squash after resolving every review conversation.

## Build, sign, and smoke-test

Create an annotated `v<version>` tag from the merged `main` commit and push it.
`.github/workflows/release.yml` then:

1. builds the desktop and CLI from the immutable tag;
2. signs both binaries through Azure Artifact Signing and GitHub OIDC;
3. packages and signs the Inno Setup installer;
4. verifies the publisher and RFC 3161 timestamps;
5. regenerates SHA-256 sidecars after signing;
6. installs, validates, and uninstalls the signed build; and
7. creates or updates a draft GitHub release; and
8. uploads the immutable installer and checksum to Cloudflare R2, then verifies
   the public download has no redirect and the expected SHA-256.

The GitHub `release` environment accepts only `v*` tags. Its federated identity
is `repo:btsouth/ceiling:environment:release`; the workflow uses environment
variables for Azure resource identifiers and does not store an Azure secret.

Microsoft Store installers are published under
`https://downloads.ceiling.win/releases/v<version>/`. The `release` environment
must define these Cloudflare values:

- variable `CLOUDFLARE_ACCOUNT_ID`;
- variable `CLOUDFLARE_R2_BUCKET` (`ceiling-downloads`); and
- secret `CLOUDFLARE_R2_API_TOKEN`, scoped to the Ceiling account with R2 object
  write access.

The release workflow runs `scripts/publish-store-installer.ps1`, which validates
the signed installer's checksum before upload and downloads it again from the
custom domain with redirects disabled. This keeps the Microsoft Store package
URL versioned, immutable, and directly downloadable. A retry skips an existing
object only when its installer and checksum bytes match; it refuses to overwrite
a version URL with different bytes.

## Microsoft Store submission automation

Microsoft Store submission is disabled until Partner Center onboarding has
been validated. The GitHub `release` environment must define:

- variable `PARTNER_CENTER_PRODUCT_ID`;
- variable `MICROSOFT_STORE_SUBMISSION_ENABLED` (`false` during onboarding);
- secrets `PARTNER_CENTER_TENANT_ID`, `PARTNER_CENTER_SELLER_ID`,
  `PARTNER_CENTER_CLIENT_ID`, and `PARTNER_CENTER_CLIENT_SECRET`.

The Entra application represented by those credentials must be associated with
the Partner Center account and assigned the **Manager** role. Ceiling must
already have a published MSI/EXE submission, and its Store product must be free;
Microsoft Store Developer CLI does not currently support updates for paid
MSI/EXE products.

After adding the credentials, run **Validate Microsoft Store submission** from
GitHub Actions with an existing R2 version and leave `publish` unchecked. This
verifies the installer and checksum, authenticates to Partner Center, reads the
current package configuration, and prepares the new package JSON without
changing the Store submission. The manual workflow uses a separate
`store-validation` environment containing the same five Partner Center values,
so it cannot access the tag-only environment's signing or R2 credentials. If it
passes:

1. rerun it once with `publish` checked to submit that version;
2. wait for certification to complete in Partner Center; and
3. set `MICROSOFT_STORE_SUBMISSION_ENABLED` to `true`.

Future tagged releases then retrieve the current Store package configuration,
change only its package URL to the newly verified immutable R2 installer, and
submit it for certification. Leave the enable variable `false` if Store
submission should temporarily remain manual. Never reuse or overwrite a
versioned R2 URL after Microsoft has certified it.

### When a release collides with an in-flight submission

The Store allows one active submission per product, so tagging a release while
the previous one is still certifying cannot submit:

```text
error - Product already has One Active Submission In-Progress. SubmissionId: <id>
```

That is a queueing conflict, not a broken release. The release run treats it as
one: the Store step reports the collision as a warning and skips, and the run
still passes, because the binaries, the GitHub release, and the R2 upload have
all already succeeded. Only the Store submission is deferred.

To submit the deferred version once the active submission clears, run
**Validate Microsoft Store submission** with `publish` checked and that version.

To clear the active submission instead, cancel it in Partner Center:

1. Apps and Games overview, then open the app.
2. On the Application overview page, go to the **Update app** card (the **App
   setup** card for a first submission).
3. Click the three dots at the top right of that card and choose
   **Cancel review**, then confirm. It returns to draft within about a minute.

It is a menu on a card rather than an action in a submissions list, which is why
it is easy to miss. Note that Ceiling is a flat MSI/EXE product: `msstore
submission delete` is documented only for MSIX, and the MSI/EXE submission API
has no delete or cancel operation at all, so Partner Center is the only way to
retract a submission. Cancelling stays available through certification and is
lost once publishing begins.

Do **not** try to resume the blocked release with `gh run rerun --failed`. A
rerun restarts the whole job rather than resuming at the failed step, so it
rebuilds and re-signs. Signing embeds an RFC 3161 timestamp, so the new
installer never matches the bytes already published for that version, and
`publish-store-installer.ps1` refuses to overwrite an immutable release object.
The rerun dies at the R2 step, long before reaching the Store.

Submit the deferred version with the validation workflow instead, which reuses
the installer already in R2 and rebuilds nothing:

```powershell
gh workflow run validate-store-submission.yml -f version=<version> -f publish=true
```

For a local unsigned packaging rehearsal, use the managed Windows checkout:

```powershell
powershell.exe -ExecutionPolicy Bypass -NoProfile -File scripts\windows-release-build.ps1 `
  -Ref v<version> `
  -SmokeInstall
```

Do not publish that local rehearsal unless its app, CLI, portable executable,
and installer have been independently signed and
`scripts\finalize-windows-release.ps1` passes.

The release directory must contain:

- `Ceiling-<version>-Setup.exe`
- `Ceiling-<version>-Setup.exe.sha256`
- `Ceiling-<version>-portable.exe`
- `Ceiling-<version>-portable.exe.sha256`

Test install, launch, tray behavior, provider refresh, the capacity strip,
autostart, and uninstall on a clean Windows user profile before publishing.

### Build cache

To keep the signed path fast, the Cargo dependency sources (`~/.cargo/registry`,
`~/.cargo/git`), the release-mode Cargo target directories, and the pnpm store
are cached with a pinned `actions/cache`, keyed by runner OS, MSVC toolchain,
resolved `rustc` version, `Cargo.lock`, and `pnpm-lock.yaml`, with prefix
restore-keys so a small dependency bump still reuses most artifacts.

GitHub Actions caches are **ref-scoped**: a tag run cannot read another tag's
cache, and only default-branch runs can write the scope every tag run can read.
So the cache is **warmed on `main`** by `.github/workflows/warm-release-cache.yml`
(on pushes that change the build graph, a weekly schedule, and manual dispatch);
`release.yml` only **restores** it. The warm workflow does the same unsigned
build and saves the cache — it does no signing and touches no secrets.

Safety properties:

- The release (tag) run only restores; it never writes a cache, so no signed
  binary, credential, signing material, or release asset is ever cached.
- The warm workflow saves only after a successful build (`success()`-gated), so
  a failed build never poisons the cache, and it saves before it would ever have
  produced a signed artifact (it never signs at all).
- A cache miss falls back to a full cold build (the original behavior), so
  caching can only make a release faster, never break it.

The release run summary reports whether the build was warm, partial, or cold;
per-phase timing is visible as the individual step durations. The first release
after a build-graph change may be partial/cold until the next warm run repopulates
the default-branch cache.

Invalidation and no-cache recovery:

- To force a clean rebuild, bump `$cacheVersion` in the **Compute release cache
  key** step of *both* `release.yml` and `warm-release-cache.yml` (keep them in
  sync), merge or push those changes to `main`, and wait for the **Warm release
  cache** workflow to complete successfully before creating and pushing the
  release tag.
- To purge stored caches, delete them from the repository's Actions cache UI or
  with `gh cache delete --all` (or a specific key). A release with no cache
  simply cold-builds.
- To warm on demand, run the **Warm release cache** workflow via *Run workflow*
  (workflow_dispatch) on `main`.

> Follow-up (evaluated, not yet applied): the build script uses separate Cargo
> target directories for the desktop and CLI builds, so the shared dependency
> graph compiles twice even on a warm run. Unifying them into one target
> directory would cut cold-build time and roughly halve the cache size; it is a
> build-script change worth measuring against the timing this cache change now
> records.

## Publish

1. Review the draft release created by the signed workflow and replace generated
   notes with concise, user-facing notes where needed.
2. Download and manually launch the signed installer and portable executable on
   a clean Windows profile.
3. Run `scripts\release-doctor.ps1 -Version <version>` and resolve all failures.
4. Publish the GitHub draft only after the manual checks pass.
5. Submit the versioned `downloads.ceiling.win` installer URL to Microsoft
   Partner Center.
6. Publish a new Winget package identity for Ceiling. Do not reuse the
   `Finesssee.Win-CodexBar` package identifier or product code.
7. Verify the GitHub download URL and SHA-256 before submitting the immutable
   Winget manifest.

## First-release gate

- The installer shows Ceiling everywhere and installs to `Programs\Ceiling`.
- No public artifact or support link points to Win-CodexBar.
- `main` is protected and the hosted checks are required.
- GitHub Issues, private vulnerability reporting, Dependabot alerts, and
  security updates are enabled.
- The README states the fork lineage and retains the upstream MIT notice.
- There are no startup notification replays or false depletion/restoration alerts.
- The five core providers have a documented happy path and a truthful failure state.
