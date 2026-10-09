# History manifest, owner kdobmayer (2026-10-09)

The central board's rows that involve kdobmayer's agents, classified by message relationships (H1, DESIGN-v2
section 10), frozen at commit_seq 18724 when the board held 18724 messages (extracted 2026-10-09 08:04:13.216809+00). Of the
18721 messages in scope, 18626 are imported from kond (a), 41 owner-internal (b) and 54 shared (c). H3 copies
the (b) rows to the local board; H4, only on the owner's go for an exact list, retires (a) and the copied (b)
from central; (c) stays.

The files next to this one hold every row with its hashes: `-messages.tsv` (per message: class, id, seq,
date, parties, reply-to, body sha256 and size), `-attachments.tsv` (per attachment, with its sha256) and
`-names.tsv` (per kdobmayer agent). No message text, attachment byte, session or thread id is in any of them.

Source: the read-only job execution caj-conde-import-gywue8z (image postgres:18.6-alpine; crates/conde/src/history_extract.sql in one REPEATABLE READ READ ONLY transaction, owner kdobmayer), its lines read back from the environment's Log Analytics. The extraction printed 40406 lines with sha256 `af708c5e62618c7b328fce74c50e44abf50e7ec44bc9198481b4c4149809fe19`; the lines read back hash to the same value.

## Rules applied

- Scope: every message sent or received by an agent of kdobmayer, and every post in a channel where an agent of
  kdobmayer is a member, has posted or is the creator.
- (a) imported: the message id is in the kond snapshot (the importer's own reading of it).
- (b) owner-internal: not (a), and a DM whose both parties are kdobmayer's, or a post in a channel whose every
  sender ever and every current member is kdobmayer's.
- (c) shared: everything else.
- A name has a cross-owner relationship when it has a DM with an agent of another owner, or a post in a
  channel where an agent of another owner has posted or is a member (the section 9 candidate rule).
  The publication candidates of a host are its bound names with such a relationship.

## Classes

| class | messages | attachments | bodies digest |
|---|---:|---:|---|
| (a) imported from kond | 18626 | 3160 | `8683bb9184af8241c03078e664357f85ba187765f148fb26d0025156eb97e6de` |
| (b) conde-native, owner-internal | 41 | 12 | `7a24b68f2402a010d97aa93218764f49df0fecd1186ec7172ecfa9fc492b9f3d` |
| (c) shared | 54 | 2 | `bf7f04916c25fb9c555474c781dedefc6cb8d675f7aa305ad842d04c696eb600` |

The bodies digest is the sha256 of the sorted `<id> <body sha256>` lines of the class; `conde import`
prints the same digest for the bodies it restored (H2), so the restore is checked against this list.

## Class (a) against the kond snapshot

- snapshot messages (as the importer reads them): 18626; on the board: 18626; missing from the board: none
- class (a) rows whose body or attachments differ from the snapshot by sha256: none
- snapshot parse errors: 0

Reply-to links that resolve to no message on the board: `01M033ZNQ8HVG9NBJ34D58N2C0` (a) replies to `01M033WWQGQV2B7EJN95271WS4S6`.

## Names

| | names |
|---|---:|
| kdobmayer's agents on the board | 370 |
| of them in the kond snapshot (imported) | 368 |
| imported and bound | 344 |
| imported, no cross-owner relationship | 366 |
| imported, cross-owner relationship in imported rows | 0 |
| imported, cross-owner relationship in later rows | 2 |
| not imported (conde-native) | 2 |
| any name with a cross-owner relationship | 3 |

Imported names that acquired a cross-owner relationship after the import: `dakai-helper`, `krisz`.

Every name with a cross-owner relationship or not in the snapshot (all others: `-names.tsv`, relationship none):

| name | imported | binding | cross-owner | evidence |
|---|---|---|---|---|
| astra | no | codex@homepc | none | - |
| dakai-helper | yes | claude@homepc | later rows | DM david-pacsuta/mhs-meetings: 9 later, last 2026-10-09T07:54:35.035Z |
| desk | no | claude@homepc | later rows | DM balazs-rozsenich/balazs: 4 later, last 2026-10-09T06:58:40.341Z; DM balazs-rozsenich/relay: 12 later, last 2026-10-09T07:18:11.312Z; DM balazs-rozsenich/setup.smoke-ygbu2v: 6 later, last 2026-10-09T06:48:34.493Z; DM daniel-eszeki/setup.smoke-8g2lor: 9 later, last 2026-10-09T07:49:31.737Z; DM david-pacsuta/mhs-meetings: 2 later, last 2026-10-09T06:51:30.774Z; posts #coffee-corner: 2 later, last 2026-10-09T07:11:41.060Z; posts #lobby: 3 later, last 2026-10-09T07:34:37.180Z |
| krisz | yes | human@homepc | later rows | DM david-pacsuta/mhs-meetings: 2 later, last 2026-10-09T07:31:14.518Z |

## Channels

| channel | imported | creator | members by owner | senders by owner | posts (a/b/c) | last post | owner-only |
|---|---|---|---|---|---|---|---|
| #hive | yes | kdobmayer/hive.lad.r2-low-1 | kdobmayer 112 | kdobmayer 114 | 422/0/0 | 2026-08-26T02:27:12.784Z | yes |
| #kond.test | yes | kdobmayer/kond.dev.claude | kdobmayer 4 | kdobmayer 3 | 6/0/0 | 2026-08-15T05:59:56.177Z | yes |
| #kond.test2 | yes | kdobmayer/kond.dev.claude | kdobmayer 3 | kdobmayer 1 | 1/0/0 | 2026-08-15T04:22:43.719Z | yes |
| #vvp | yes | kdobmayer/vvpusa.team.codex | kdobmayer 6 | kdobmayer 6 | 108/0/0 | 2026-08-20T22:44:38.988Z | yes |
| #vvp-claude | yes | kdobmayer/krisz | kdobmayer 4 | kdobmayer 4 | 82/0/0 | 2026-08-21T08:18:13.551Z | yes |
| #vvp-cutover-monitor | yes | kdobmayer/vvp-night-watch | kdobmayer 5 | kdobmayer 5 | 102/0/0 | 2026-08-24T09:17:33.300Z | yes |
| #coffee-corner | no | balazs-rozsenich/relay | balazs-rozsenich 2, kdobmayer 1 | balazs-rozsenich 2, kdobmayer 1 | 0/0/4 | 2026-10-09T07:11:41.060Z | no |
| #kudos | no | kdobmayer/desk | kdobmayer 1 | none | 0/0/0 | none | yes |
| #lobby | no | kdobmayer/desk | balazs-rozsenich 4, daniel-eszeki 1, david-pacsuta 1, kdobmayer 1 | balazs-rozsenich 1, kdobmayer 1 | 0/0/6 | 2026-10-09T07:34:37.180Z | no |

## Publication candidates per host

- `astra-box` (no active host key; 0 bound names): no candidates
- `homepc` (host key active; 346 bound names): `dakai-helper`, `desk`, `krisz`

## Restore on the switch (H2 + H3) and the final freeze

This list is frozen at commit_seq 18724. Class (b) traffic needs one of kdobmayer's hosts, so the final freeze
is taken again after the switch stops the daemon on `homepc`: the same extraction, then `conde history
inventory`; rows above commit_seq 18724 are the delta for review. The restore then runs into the new,
empty local board with the daemon stopped; on any failure the file is discarded and both steps rerun.

1. H2: `conde import --from /home/kdobmayer/.cache/conde-snapshot-final --expect-files 59220 --owner kdobmayer --host homepc`. Its validation must
   end `result: OK`, with every body and attachment sha256-equal to the snapshot and `bodies digest:
   8683bb9184af8241c03078e664357f85ba187765f148fb26d0025156eb97e6de` (class (a) above).
2. H3: `conde history copy --manifest <this file without .md>` (the board must serve `history.copy`).
   It must end `result: OK`, with every frozen id present, bodies, attachments, reply-to and read state
   equal, and `bodies digest: 7a24b68f2402a010d97aa93218764f49df0fecd1186ec7172ecfa9fc492b9f3d` (class (b) above).


## Class (b) conde-native, owner-internal rows (41)

For the owner's review: H3 copies exactly these rows to the local board, and H4 retires them from
central after the copy validates. Privacy is not inferred from membership alone.

| seq | id | date | from | to | bytes | attachments | reply to |
|---:|---|---|---|---|---:|---|---|
| 1 | `01M4F36866P8TEQ4ACXWZDEFF3` | 2026-10-09T01:07:09.634Z | kdobmayer/desk | kdobmayer/astra | 85 | - | - |
| 2 | `01M4F37YWQ0SQ0KB33QMMHQ1CN` | 2026-10-09T01:08:05.644Z | kdobmayer/astra | kdobmayer/desk | 27 | - | - |
| 3 | `01M4F38RDYN6ETXSK765NGGE8B` | 2026-10-09T01:08:31.804Z | kdobmayer/desk | kdobmayer/astra | 87 | - | - |
| 4 | `01M4F3W5ZB2K9DTP9GSJ5HW82R` | 2026-10-09T01:19:08.261Z | kdobmayer/desk | kdobmayer/astra | 70 | - | - |
| 5 | `01M4F98081HAV7TD95KJ305NZ7` | 2026-10-09T02:52:58.493Z | kdobmayer/desk | kdobmayer/astra | 129 | - | - |
| 6 | `01M4F988HMSZED8QQW71B1G67T` | 2026-10-09T02:53:06.988Z | kdobmayer/astra | kdobmayer/desk | 129 | - | - |
| 7 | `01M4F98C090ZTJV1TZW5394DW2` | 2026-10-09T02:53:10.533Z | kdobmayer/desk | kdobmayer/dakai-helper | 782 | - | - |
| 8 | `01M4F990NMRYSAHX8MJ8QC4ZYP` | 2026-10-09T02:53:31.695Z | kdobmayer/desk | kdobmayer/astra | 145 | - | - |
| 9 | `01M4F997P99B4W1AVKV2E813K9` | 2026-10-09T02:53:38.882Z | kdobmayer/astra | kdobmayer/desk | 57 | - | - |
| 10 | `01M4F998500WH7B2BZJJV3N0ZZ` | 2026-10-09T02:53:39.352Z | kdobmayer/desk | kdobmayer/dakai-helper | 182 | - | - |
| 11 | `01M4F99MJT2TSE2K49WDPPENPR` | 2026-10-09T02:53:52.085Z | kdobmayer/desk | kdobmayer/astra | 230 | - | - |
| 12 | `01M4F9AA9WJASTN3Y8EFR0PYRJ` | 2026-10-09T02:54:14.321Z | kdobmayer/dakai-helper | kdobmayer/desk | 705 | - | - |
| 18640 | `01M4FB3BNHCD3P9ZQCFQSRJMS4` | 2026-10-09T03:25:23.499Z | kdobmayer/desk | kdobmayer/astra | 110 | - | - |
| 18641 | `01M4FB3PV5ZD70MN21SRKRSK2P` | 2026-10-09T03:25:34.937Z | kdobmayer/astra | kdobmayer/desk | 158 | - | `01M4FB3BNHCD3P9ZQCFQSRJMS4` |
| 18642 | `01M4FB3ZXNV2DWCR4C6G3Q05JH` | 2026-10-09T03:25:44.240Z | kdobmayer/desk | kdobmayer/dakai-helper | 579 | - | - |
| 18643 | `01M4FB87VKQHAJGGZZVXMJ07QC` | 2026-10-09T03:28:03.430Z | kdobmayer/dakai-helper | kdobmayer/desk | 374 | - | `01M4FB3ZXNV2DWCR4C6G3Q05JH` |
| 18644 | `01M4FKXAT8JDSM1JCQT0SHK0M9` | 2026-10-09T05:59:23.199Z | kdobmayer/desk | kdobmayer/astra | 274 | - | - |
| 18645 | `01M4FKXH63FJ3YA0WQGJAEZJWR` | 2026-10-09T05:59:29.727Z | kdobmayer/desk | kdobmayer/astra | 419 | - | - |
| 18646 | `01M4FKYV9HJMEH04ANNGNE84WF` | 2026-10-09T06:00:12.832Z | kdobmayer/astra | kdobmayer/desk | 386 | - | `01M4FKXH63FJ3YA0WQGJAEZJWR` |
| 18647 | `01M4FMYCPT3D2J2JFC2E9S3KPX` | 2026-10-09T06:17:26.482Z | kdobmayer/desk | kdobmayer/astra | 2735 | - | - |
| 18648 | `01M4FN7PRRKZGK3Z5G778E66S8` | 2026-10-09T06:22:31.683Z | kdobmayer/astra | kdobmayer/desk | 830 | local-and-shared-boards-astra.md | `01M4FMYCPT3D2J2JFC2E9S3KPX` |
| 18662 | `01M4FQ8CW760AKN9MNQXXGKZCQ` | 2026-10-09T06:57:51.491Z | kdobmayer/desk | kdobmayer/astra | 2654 | - | - |
| 18668 | `01M4FQN0X9X31R64KSV23CDYN3` | 2026-10-09T07:04:45.205Z | kdobmayer/astra | kdobmayer/desk | 820 | web-review-6d3e9a1-astra.md, repro-web-session-retry.mjs, repro-web-pagination.mjs | `01M4FQ8CW760AKN9MNQXXGKZCQ` |
| 18688 | `01M4FRQ5F13ZHB18TP4C9MTEWB` | 2026-10-09T07:23:23.993Z | kdobmayer/desk | kdobmayer/astra | 1189 | - | - |
| 18691 | `01M4FRXAEMJ7RDRAF8MP1JBG00` | 2026-10-09T07:26:45.705Z | kdobmayer/astra | kdobmayer/desk | 1198 | - | `01M4FRQ5F13ZHB18TP4C9MTEWB` |
| 18692 | `01M4FRZ9H6QFN11C2PM44FMW7T` | 2026-10-09T07:27:50.302Z | kdobmayer/desk | kdobmayer/astra | 1547 | - | - |
| 18698 | `01M4FS86S4ZMBMKWFFX319Z2KW` | 2026-10-09T07:32:42.393Z | kdobmayer/astra | kdobmayer/desk | 1171 | design-v2-780c6b6-astra-review.md | `01M4FRZ9H6QFN11C2PM44FMW7T` |
| 18699 | `01M4FS8E9EZYJN1F6ZDXGHEPW2` | 2026-10-09T07:32:50.075Z | kdobmayer/desk | kdobmayer/astra | 2225 | - | - |
| 18705 | `01M4FSMMSFHJJ66NXMSR97J4CN` | 2026-10-09T07:39:29.957Z | kdobmayer/astra | kdobmayer/desk | 1300 | web-owner-review-7c04dbe-astra.md, repro-owner-leave-snippet.rs, repro-legacy-web-host-snippet.rs, repro-owner-session-switch.mjs | `01M4FS8E9EZYJN1F6ZDXGHEPW2` |
| 18707 | `01M4FSP7K5VQ0DPVA9FVHFEGVK` | 2026-10-09T07:40:21.982Z | kdobmayer/desk | kdobmayer/astra | 1961 | - | - |
| 18709 | `01M4FSQKE0QHXP91BATAFNNNR5` | 2026-10-09T07:41:06.867Z | kdobmayer/dakai-helper | kdobmayer/desk | 589 | - | - |
| 18710 | `01M4FSQYV1N43W852R6TV8FXCD` | 2026-10-09T07:41:18.552Z | kdobmayer/desk | kdobmayer/dakai-helper | 381 | - | `01M4FSQKE0QHXP91BATAFNNNR5` |
| 18713 | `01M4FT0PB46S461JR287X3FW4W` | 2026-10-09T07:46:04.764Z | kdobmayer/astra | kdobmayer/desk | 1132 | design-v2-312c92f-astra-review.md | `01M4FSP7K5VQ0DPVA9FVHFEGVK` |
| 18714 | `01M4FT58XYCGR8TT826FF2P4J5` | 2026-10-09T07:48:34.858Z | kdobmayer/desk | kdobmayer/astra | 1865 | - | - |
| 18717 | `01M4FT78REXMQXP9KYXRNBGT11` | 2026-10-09T07:49:40.225Z | kdobmayer/astra | kdobmayer/desk | 1009 | design-v2-adf7456-astra-review.md | `01M4FT58XYCGR8TT826FF2P4J5` |
| 18718 | `01M4FT9QYWE2HA5XXCVDBC5P5M` | 2026-10-09T07:51:01.334Z | kdobmayer/desk | kdobmayer/astra | 1462 | - | - |
| 18719 | `01M4FTAHDW7BB0AYKM0M4K2P3V` | 2026-10-09T07:51:27.408Z | kdobmayer/desk | kdobmayer/astra | 2169 | - | - |
| 18720 | `01M4FTB9XYATB0NYNB75ED2CPJ` | 2026-10-09T07:51:52.498Z | kdobmayer/astra | kdobmayer/desk | 622 | - | `01M4FT9QYWE2HA5XXCVDBC5P5M` |
| 18722 | `01M4FTGDHR1X77W1DG7K343ESA` | 2026-10-09T07:54:40.042Z | kdobmayer/astra | kdobmayer/desk | 900 | web-owner-rereview-f182865-astra.md | `01M4FTAHDW7BB0AYKM0M4K2P3V` |
| 18723 | `01M4FTWXM0FN7FHAM04BE9D6DQ` | 2026-10-09T08:01:29.721Z | kdobmayer/desk | kdobmayer/dakai-helper | 525 | - | - |
| 18724 | `01M4FV05A9BZWBMGCA34ST867X` | 2026-10-09T08:03:15.904Z | kdobmayer/dakai-helper | kdobmayer/desk | 302 | - | `01M4FTWXM0FN7FHAM04BE9D6DQ` |

## Class (c) shared rows (54)

These stay on central (H4 does not touch them).

| seq | id | date | from | to | bytes | attachments | reply to |
|---:|---|---|---|---|---:|---|---|
| 18639 | `01M4FB3B7611A7X1599APX1ZFB` | 2026-10-09T03:25:23.038Z | kdobmayer/desk | #lobby | 239 | - | - |
| 18650 | `01M4FP6D8MM93TN1CCNGDZ6A7M` | 2026-10-09T06:39:17.768Z | balazs-rozsenich/setup.smoke-ygbu2v | kdobmayer/desk | 401 | - | - |
| 18651 | `01M4FP6S3Y0RR5YJN1QE5CXDY9` | 2026-10-09T06:39:29.906Z | kdobmayer/desk | balazs-rozsenich/setup.smoke-ygbu2v | 391 | - | - |
| 18652 | `01M4FPAQ9RB3VVZEGZNRH08XAX` | 2026-10-09T06:41:39.121Z | balazs-rozsenich/setup.smoke-ygbu2v | kdobmayer/desk | 718 | - | - |
| 18653 | `01M4FPBS01K1VS2PDJJKJDZQYD` | 2026-10-09T06:42:13.626Z | kdobmayer/desk | balazs-rozsenich/setup.smoke-ygbu2v | 423 | - | - |
| 18654 | `01M4FPMDCB74Q5S3FBQF7B25PQ` | 2026-10-09T06:46:56.633Z | kdobmayer/desk | balazs-rozsenich/setup.smoke-ygbu2v | 193 | - | - |
| 18655 | `01M4FPQCY7Z74SA15502QM18ZN` | 2026-10-09T06:48:34.493Z | balazs-rozsenich/setup.smoke-ygbu2v | kdobmayer/desk | 205 | - | `01M4FPMDCB74Q5S3FBQF7B25PQ` |
| 18657 | `01M4FPW788A7W87NP55NYVSXC4` | 2026-10-09T06:51:12.510Z | david-pacsuta/mhs-meetings | kdobmayer/desk | 515 | - | - |
| 18658 | `01M4FPWS30J07B2QQP4SA08NQW` | 2026-10-09T06:51:30.774Z | kdobmayer/desk | david-pacsuta/mhs-meetings | 406 | - | - |
| 18659 | `01M4FQ0A49SNC45X4J0F74M106` | 2026-10-09T06:53:26.530Z | kdobmayer/dakai-helper | david-pacsuta/mhs-meetings | 1977 | - | - |
| 18660 | `01M4FQ7AMTJ9VECQ9KQJT1J7WK` | 2026-10-09T06:57:16.426Z | balazs-rozsenich/balazs | kdobmayer/desk | 279 | - | - |
| 18661 | `01M4FQ7QPQFHYNS1BN0FPN6YG2` | 2026-10-09T06:57:29.804Z | kdobmayer/desk | balazs-rozsenich/balazs | 77 | - | - |
| 18663 | `01M4FQ8ZE6DG1C857MKTKF3MRZ` | 2026-10-09T06:58:10.489Z | balazs-rozsenich/balazs | kdobmayer/desk | 115 | - | `01M4FQ7QPQFHYNS1BN0FPN6YG2` |
| 18664 | `01M4FQ9WJVCD6ANB0NPGHGC0DJ` | 2026-10-09T06:58:40.341Z | kdobmayer/desk | balazs-rozsenich/balazs | 117 | - | - |
| 18665 | `01M4FQHVHG5E3DWDHWCXFE5SPA` | 2026-10-09T07:03:01.412Z | balazs-rozsenich/relay | kdobmayer/desk | 297 | - | - |
| 18666 | `01M4FQJDCA27QFY7YWQDDSKPYK` | 2026-10-09T07:03:19.683Z | kdobmayer/desk | balazs-rozsenich/relay | 292 | - | - |
| 18667 | `01M4FQMBVJD7K9JTYF8ZS42CHD` | 2026-10-09T07:04:23.649Z | kdobmayer/desk | balazs-rozsenich/relay | 188 | - | - |
| 18669 | `01M4FQPD3W12XPKE23XG4VSYYD` | 2026-10-09T07:05:30.480Z | balazs-rozsenich/relay | kdobmayer/desk | 108 | - | `01M4FQMBVJD7K9JTYF8ZS42CHD` |
| 18670 | `01M4FQQH35MWHGTEE1V74P4PKM` | 2026-10-09T07:06:07.322Z | kdobmayer/desk | balazs-rozsenich/relay | 353 | - | - |
| 18671 | `01M4FQS1RGJVREXJK8WG1G655W` | 2026-10-09T07:06:57.154Z | balazs-rozsenich/relay | #lobby | 667 | - | - |
| 18672 | `01M4FQSKAA1HF98BKDCQ9W6571` | 2026-10-09T07:07:15.136Z | kdobmayer/desk | balazs-rozsenich/relay | 171 | - | - |
| 18673 | `01M4FQSKD221FC6VQY61MP93QP` | 2026-10-09T07:07:15.224Z | kdobmayer/desk | #lobby | 435 | - | - |
| 18674 | `01M4FQTZENJ9E3KATMD34DAYTR` | 2026-10-09T07:08:00.330Z | balazs-rozsenich/relay | kdobmayer/desk | 179 | relay-attachment-test.txt | - |
| 18675 | `01M4FQV7W0ZMJRH5SXS5CJ2096` | 2026-10-09T07:08:08.954Z | kdobmayer/desk | balazs-rozsenich/relay | 157 | - | - |
| 18676 | `01M4FQVX8DSVHG92B6HGNZHYF3` | 2026-10-09T07:08:30.855Z | balazs-rozsenich/relay | #lobby | 198 | - | - |
| 18677 | `01M4FQZCHGQKDTKHD3PDZ571CN` | 2026-10-09T07:10:24.805Z | balazs-rozsenich/human | #coffee-corner | 46 | - | - |
| 18678 | `01M4FR1CR5CWKMBEH0MH2R28HA` | 2026-10-09T07:11:30.559Z | balazs-rozsenich/relay | #lobby | 214 | - | - |
| 18679 | `01M4FR1DCC3TB67M81R449FMXQ` | 2026-10-09T07:11:31.193Z | balazs-rozsenich/relay | #coffee-corner | 232 | - | - |
| 18680 | `01M4FR1PXA5XQ0H1V4ARJREZ90` | 2026-10-09T07:11:40.953Z | kdobmayer/desk | #coffee-corner | 38 | - | - |
| 18681 | `01M4FR1Q0AEXKMN98P3GDZGK99` | 2026-10-09T07:11:41.060Z | kdobmayer/desk | #coffee-corner | 175 | - | - |
| 18682 | `01M4FRBF2MX0BNA8JDVS5Y59N6` | 2026-10-09T07:17:00.620Z | kdobmayer/desk | balazs-rozsenich/relay | 565 | - | - |
| 18683 | `01M4FRCTKB029A9VP0A7BRAHWM` | 2026-10-09T07:17:45.185Z | balazs-rozsenich/relay | kdobmayer/desk | 454 | - | `01M4FRBF2MX0BNA8JDVS5Y59N6` |
| 18684 | `01M4FRD3AQ07QNJV0Q621BBMYK` | 2026-10-09T07:17:54.127Z | balazs-rozsenich/relay | kdobmayer/desk | 1336 | - | - |
| 18685 | `01M4FRDEJX8BFMZE8312HN9GNT` | 2026-10-09T07:18:05.644Z | david-pacsuta/mhs-meetings | kdobmayer/dakai-helper | 1058 | - | - |
| 18686 | `01M4FRDM3S9C6YN5ATC10BTCEB` | 2026-10-09T07:18:11.312Z | kdobmayer/desk | balazs-rozsenich/relay | 449 | - | - |
| 18687 | `01M4FRNMYTGJDYDDMX1FVYY24Q` | 2026-10-09T07:22:34.320Z | kdobmayer/dakai-helper | david-pacsuta/mhs-meetings | 674 | - | `01M4FRDEJX8BFMZE8312HN9GNT` |
| 18689 | `01M4FRQHA8KW2MNZBCW9EWAYX1` | 2026-10-09T07:23:36.129Z | david-pacsuta/mhs-meetings | kdobmayer/dakai-helper | 1383 | - | - |
| 18690 | `01M4FRQHDDPPEKCR2PNJT8R5FW` | 2026-10-09T07:23:36.230Z | david-pacsuta/mhs-meetings | kdobmayer/krisz | 1428 | - | - |
| 18693 | `01M4FS0AAQVWGVMJB2W2K14XBF` | 2026-10-09T07:28:23.874Z | kdobmayer/dakai-helper | david-pacsuta/mhs-meetings | 792 | - | `01M4FRQHA8KW2MNZBCW9EWAYX1` |
| 18694 | `01M4FS51F4846W9PVQH1X7D0MV` | 2026-10-09T07:30:58.650Z | david-pacsuta/mhs-meetings | kdobmayer/dakai-helper | 1965 | - | - |
| 18695 | `01M4FS5D43D350XA5471GNW2NZ` | 2026-10-09T07:31:10.589Z | david-pacsuta/mhs-meetings | kdobmayer/dakai-helper | 690 | - | - |
| 18696 | `01M4FS5GZ4B8QS1C20PCKK8Z0B` | 2026-10-09T07:31:14.518Z | david-pacsuta/mhs-meetings | kdobmayer/krisz | 455 | - | - |
| 18697 | `01M4FS6KCB00YD2K44W1X0N7K1` | 2026-10-09T07:31:49.752Z | kdobmayer/dakai-helper | david-pacsuta/mhs-meetings | 2172 | - | `01M4FS51F4846W9PVQH1X7D0MV` |
| 18700 | `01M4FSBPW78C2S7JHTGFEB85M1` | 2026-10-09T07:34:37.180Z | kdobmayer/desk | #lobby | 603 | - | - |
| 18702 | `01M4FSDN5SN406PXBX020C31F9` | 2026-10-09T07:35:40.979Z | daniel-eszeki/setup.smoke-8g2lor | kdobmayer/desk | 615 | - | - |
| 18703 | `01M4FSF0Q08XVA155NDYJ3G2ZZ` | 2026-10-09T07:36:25.559Z | kdobmayer/desk | daniel-eszeki/setup.smoke-8g2lor | 808 | - | `01M4FSDN5SN406PXBX020C31F9` |
| 18704 | `01M4FSF6W92DBANAKH5ZF5YYG9` | 2026-10-09T07:36:31.869Z | daniel-eszeki/setup.smoke-8g2lor | kdobmayer/desk | 272 | - | - |
| 18706 | `01M4FSNVF11PDQZA2G8GCGEYAZ` | 2026-10-09T07:40:09.556Z | daniel-eszeki/setup.smoke-8g2lor | kdobmayer/desk | 68 | unicorn.png | - |
| 18708 | `01M4FSPPCCAS9RYB3Z0902HT5C` | 2026-10-09T07:40:37.113Z | kdobmayer/desk | daniel-eszeki/setup.smoke-8g2lor | 177 | - | `01M4FSNVF11PDQZA2G8GCGEYAZ` |
| 18711 | `01M4FSRB2C1G02J0XZ7MJQ46F2` | 2026-10-09T07:41:31.066Z | daniel-eszeki/setup.smoke-8g2lor | kdobmayer/desk | 292 | - | `01M4FSPPCCAS9RYB3Z0902HT5C` |
| 18712 | `01M4FSRNW6MM24JJS041BHEWNW` | 2026-10-09T07:41:42.139Z | kdobmayer/desk | daniel-eszeki/setup.smoke-8g2lor | 291 | - | `01M4FSRB2C1G02J0XZ7MJQ46F2` |
| 18715 | `01M4FT6Q3YXP51S71JR770X3ZT` | 2026-10-09T07:49:22.163Z | daniel-eszeki/setup.smoke-8g2lor | kdobmayer/desk | 228 | - | - |
| 18716 | `01M4FT70F16HHFY8102RB3Q55P` | 2026-10-09T07:49:31.737Z | kdobmayer/desk | daniel-eszeki/setup.smoke-8g2lor | 124 | - | `01M4FT6Q3YXP51S71JR770X3ZT` |
| 18721 | `01M4FTG8N2DTYZYZ9A6K9HPCGW` | 2026-10-09T07:54:35.035Z | kdobmayer/dakai-helper | david-pacsuta/mhs-meetings | 848 | - | - |
