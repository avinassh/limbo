Your goal is to find bugs in ATTACH implementation. It is experimental, so obviously there will still be many bugs.  You also have access to sqlite source code at /home/ubuntu/sqlite

Not only in the ATTACH implementation itself, but also in its interactions with other features of the DB (transactions, triggers, updates ON ..., replace into, mvcc, integrity check, check constraints, strict tables, json, checksums, encryption etc.)

some common possible bugs: bugs when we do writes on attached db, reading attached dbs, reading / writing their schemas etc. 

attach has a safe guard to not to allow attach of different page size than main

You are only allowed to use SQL statements with the CLI (`cargo run --bin tursodb -q -- -m list -q`), and if you need them, sql files in the `/tmp/claude/custom_types/` directory. 

You will need to be teleological, resourceful, mission-oriented, analytical, cartesian, and use your reasoning powers for this.

Keep a log of what you've tried in `things_tried.md` in the project root. 

If you find unrelated bugs, write them down in `unrelated_bugs.md`, and keep going. Whenever you find a bug, log it in `attach_bugs.md`, and keep find more bugs.

Do not run the existing tests, fuzzers, simulations, or any oracles from this project. They will not help you find the bugs. It is forbidden to run them.

If you find a FIXME or TODO in the code, it implies that the devs are aware of the condition, so don't spend too much time on it but note them down in the bugs file. If you choose to run long fuzz tests, run them in the background while you continue investigating.

Do NOT fix any bugs. Your goal is solely to investigate and gather as many bugs as possible.  go through all the issues yourself. they can be duplicate, same, or extension of another. review and merge them.

never delete contents of things_tried.md or attach_bugs.md, always amend / edit them if required. and keep adding new stuff at the bottom.

once you find 5 issues, commit the files and just exit.
