# Schedules and automation

Shprotify can scan for new files, refresh personal feeds and look up
metadata for selected albums or tracks. The server runs these schedules
for you.

| What to schedule | Who sets it up | What happens |
| --- | --- | --- |
| Library scan | An administrator in the server settings | New files appear in the shared catalog. |
| Feed refresh | The owner of the feed | The chosen feed gets fresh suggestions when the source finds them. |
| Metadata lookup | An administrator for a chosen artist, album or track | New suggestions are available to review and apply. |

## Enable schedules and library scans

The administrator turns on schedules with `automation.enabled`.
This example also scans music folders every hour.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_AUTOMATION__ENABLED: "true"
SHPROTIFY_AUTOMATION__LIBRARY__SCAN_INTERVAL_SECONDS: "3600"
```
```toml [TOML]
[automation]
enabled = true

[automation.library]
scan_interval_seconds = 3600
```
:::

Shprotify scans the folders you added to the library and skips files
that haven't changed. If you only want schedules for feeds or metadata,
leave `scan_interval_seconds = 0`.
[How to apply settings](./configuration.md)

## Refresh a personal feed

Start by [creating a feed](./discovery.md). If your client supports
schedules, open the feed settings and choose an interval. The default
is six hours. With the standard server settings, you can choose from one
hour to 30 days. Each feed has its own schedule.

::: details Set a feed schedule manually in Swagger
[Sign in with your account](./quick-start.md#sign-in) and open **Discovery**.

1. Run `GET /api/v1/me/discovery/feeds/{id}/schedule` with the feed ID.
   Note the `version` and check the allowed interval range in the response.
2. Run `PUT /api/v1/me/discovery/feeds/{id}/schedule` with the same ID.
   Use the version from the response. This example refreshes a new feed
   every six hours (`21600` seconds).

   ```json
   {"version":0,"enabled":true,"interval_seconds":21600}
   ```

3. Check `next_run_at` in the response. To turn off this feed's schedule,
   send the same request with the current version and `enabled: false`.
:::

## Schedule metadata lookups

First [enable MusicBrainz lookup](./metadata.md), then allow scheduled
metadata checks. Keep the main `automation.enabled` switch on.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_AUTOMATION__METADATA__ENABLED: "true"
```
```toml [TOML]
[automation.metadata]
enabled = true
```
:::

After applying the settings, choose a particular artist, album or track
in a client that can manage metadata and enable a schedule for it. The
suggested interval is one day. That's also the minimum until the
administrator changes it.

::: details Set a metadata schedule manually in Swagger
[Sign in as an administrator](./quick-start.md#sign-in). Under
**Admin: Automation**, run `GET /api/v1/metadata/{kind}/{id}/schedule`.
Enter the catalog ID and its kind: `artist`, `release` or `recording`.

Then run `PUT /api/v1/metadata/{kind}/{id}/schedule` with the same ID and
kind. Use the `version` returned by GET and set the interval in seconds.

```json
{"version":0,"enabled":true,"interval_seconds":86400}
```
:::

> [!NOTE]
> Shprotify looks up the confirmed MusicBrainz ID or searches by name.
> An administrator reviews the suggestions. While there are suggestions
> to review, the next automatic lookup waits. [How to review suggestions](./metadata.md)

## When jobs run

A new schedule's first run takes place after the configured interval.
The next interval starts after a successful job finishes. Shprotify
checks for due jobs roughly every 30 seconds. If the server was offline,
it won't launch a separate job for every missed interval.

If a schedule didn't run as expected, check the next run time
(`next_run_at`) and latest status (`last_status`). The `paused_reason`
or `message` field may explain what happened, such as a disabled provider
or a suggestion still awaiting review. After a failure, the server may wait
longer than usual before retrying. Administrators can check the
scan schedule under **Admin: Automation** with `GET /api/v1/automation`.

To pause all schedules, set `automation.enabled = false` and apply the
server settings. Saved intervals are kept. You can still start scans,
feed refreshes and metadata lookups by hand.
[All scheduling settings](./configuration.md#complete-reference)
