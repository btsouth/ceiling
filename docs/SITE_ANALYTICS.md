# Ceiling site analytics

Ceiling's Cloudflare Worker serves the static site, records a small set of first-party events, and protects the private dashboard at `https://ceiling.win/admin`.

## What the dashboard measures

- Website visitors, pageviews, download clicks, paths, and referrers for the trailing 30 days through PostHog.
- Cumulative GitHub release asset downloads, split between installer and portable builds.
- Repository stars, forks, and open issues plus pull requests.
- GitHub repository visitors, clones, referrers, and popular paths for GitHub's trailing 14-day window.

GitHub release downloads are download events, not unique users. GitHub does not expose historical repository traffic beyond its rolling 14-day window.

## Privacy

The landing page does not load a third-party analytics script or set analytics cookies. It sends pageview and link-click events to Ceiling's own Worker. The Worker creates a pseudonymous visitor identifier by hashing the request IP, user agent, a private salt, and a 30-day rotation period before sending the event to PostHog. The identifier changes every 30 days; raw IP addresses and user agents are not included in the analytics event. Counts that span a rotation boundary may count the same visitor twice.

## Cloudflare configuration

Set these Worker secrets:

```powershell
npx wrangler secret put ADMIN_TOKEN
npx wrangler secret put ANALYTICS_SALT
npx wrangler secret put POSTHOG_PUBLIC_KEY
npx wrangler secret put POSTHOG_QUERY_KEY
npx wrangler secret put POSTHOG_PROJECT_ID
npx wrangler secret put GITHUB_TOKEN
```

Set these Worker variables in the Cloudflare dashboard, or add non-secret values to the production environment configuration:

| Variable | Purpose |
| --- | --- |
| `POSTHOG_QUERY_HOST` | Optional. Defaults to `https://us.posthog.com`. |
| `POSTHOG_CAPTURE_HOST` | Optional. Defaults to `https://us.i.posthog.com`. |
| `GITHUB_REPO` | Optional. Defaults to `btsouth/ceiling`. |

Production currently uses the shared SouthForge PostHog project (`493711`) with a pinned [Ceiling dashboard](https://us.posthog.com/project/493711/dashboard/1846024). Ceiling events and queries are isolated with `$host = 'ceiling.win'` and `source = 'ceiling.win'`; Toolport events remain separate. `POSTHOG_PUBLIC_KEY` is the project's `phc_...` capture key. `POSTHOG_QUERY_KEY` is a personal `phx_...` API key with Query Read access to the project.

The public key, numeric project ID, and a generated analytics salt are configured as Cloudflare Worker secrets. The PostHog MCP OAuth connection cannot create or reveal personal API keys, so `POSTHOG_QUERY_KEY` must be created from [PostHog user API key settings](https://us.posthog.com/settings/user-api-keys) and entered directly with `wrangler secret put`; do not paste it into an issue or commit it.

For `GITHUB_TOKEN`, use a fine-grained token limited to the Ceiling repository with **Administration: Read** permission. GitHub's public API does not need this token for release counts, but the protected Traffic API does.

Generate long random values for `ADMIN_TOKEN` and `ANALYTICS_SALT`; do not commit either value. If PostHog is not configured, GitHub release and repository totals still work and the dashboard shows a setup note in place of website traffic. If the GitHub token is absent, public repository and download totals still work while the 14-day traffic panel remains disabled.

Cloudflare pull-request previews do not receive production secrets, so the shared `wrangler.jsonc` cannot declare them as required without breaking every preview build. After a production deployment, verify the active Worker version still lists all six secret names and that `/admin` returns the protected login page before treating the deployment as healthy.

## Local validation

Create a `.dev.vars` file that is excluded from git:

```text
ADMIN_TOKEN=local-test-token
ANALYTICS_SALT=local-test-salt
POSTHOG_PUBLIC_KEY=phc_example
POSTHOG_QUERY_KEY=phx_example
POSTHOG_PROJECT_ID=12345
GITHUB_TOKEN=github_pat_example
```

Then run:

```powershell
npx wrangler dev
```

Open `http://localhost:8787/admin`. Never commit `.dev.vars`.
