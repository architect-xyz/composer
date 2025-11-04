# Support timezones for cron schedules

The cron crate used by this app does support timezones from `chrono-tz` when computing schedules.  Let's enable the user to optionally specify timezones and utilize them.

I want the label controlling the timezone to be `co.architect.composer.tz=America/Chicago`, for example.

If the label isn't specified, it should default to Utc as it does now.

Add a unit test somewhere that ensures that the upcoming schedule looks correct over a timezone boundary (e.g. daylight savings time change)