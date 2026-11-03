# slop_ninja_pool_components

Groups an admitted human pool's source groups into contributor-closure
components, the split and bootstrap unit of the A protocol: groups that share
a `writer_key` (for example one Stack Exchange user) or a dedup
`pool_admission.split_component` are merged by transitive closure.

```sh
slop_ninja_pool_components metadata.jsonl --out DIR
```

It reads text-free metadata rows only and refuses a row that carries `text`.
It writes `components.jsonl` (source group, document count, component key) and
`summary.json` (size histogram, largest components, per-site counts). The
component key is the smallest member source-group key.

Contributor identity can be widened before merging:

```sh
slop_ninja_7z_extract cat site.7z Users.xml | slop_ninja_pool_components se-accounts --site SITE > aliases.jsonl
slop_ninja_pool_components metadata.jsonl --out DIR --writer-alias aliases.jsonl
```

`se-accounts` reads only each user row's `Id` and `AccountId` and maps the
per-site `writer_key` to `stackexchange_network:account:N`, so one person's
accounts on several sites form one contributor.
