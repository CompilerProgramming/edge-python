# Runbook

## Production approval

Secrets are repository secrets, kept in one place. The pause before production is the `production` environment under Settings, Environments, with `dylan-sutton-chavez` as its required reviewer, so a `v` tag waits on Ship until the run is approved.

## What a deploy keeps

A push to `main` promotes to dev and a `v` tag ships to production, and either one replaces the files the last build shipped.

- Dev rebuilds its database from the schema and the seed, and sweeps the packages published under `pkg/` with it.
- Production only runs the migrations its database has not recorded, and keeps every published package.
- A frozen release, the copy a tag from `v1.0.0` keeps under its version, stays in both.

## Migrations

A schema change edits `site/db/schema.sql` and adds its step to `site/db/migrations/` in the same commit. Production runs the step on the next `v` tag, and after that release you delete the file by hand, which the Database job warns about until you do.
