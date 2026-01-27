# VACUUM Implementation Plan for Turso DB

## Table of Contents
1. [Overview](#overview)
2. [Locking, Concurrency, and Blocking Behavior](#locking-concurrency-and-blocking-behavior)
3. [SQLite's VACUUM Implementation Deep Dive](#sqlites-vacuum-implementation-deep-dive)
4. [SQLite's Auto-Vacuum and Incremental Vacuum](#sqlites-auto-vacuum-and-incremental-vacuum)
5. [Turso DB Current Architecture](#turso-db-current-architecture)
6. [ATTACH Database Support](#attach-database-support)
7. [Implementation Plan](#implementation-plan)
8. [Edge Cases and Gotchas](#edge-cases-and-gotchas)
9. [Testing Strategy](#testing-strategy)
10. [References](#references)
11. [Appendix: Complete B-tree Dependencies in Turso DB](#appendix-complete-b-tree-dependencies-in-turso-db)

---

## Overview

VACUUM is a database maintenance command that rebuilds the database file, repacking it to reclaim space and defragment the file. SQLite's VACUUM serves several purposes:

1. **Reclaim free space** - When rows are deleted or tables dropped, the space is marked as free but not returned to the OS
2. **Defragmentation** - Rebuild B-trees to be more compact and sequential
3. **Page size changes** - Apply pending `PRAGMA page_size` changes
4. **Auto-vacuum mode changes** - Apply pending `PRAGMA auto_vacuum` changes
5. **Database file repair** - Fix minor corruption by rebuilding from logical contents

### VACUUM Variants

```sql
VACUUM;                      -- Vacuum the main database
VACUUM schema_name;          -- Vacuum a specific attached database
VACUUM INTO 'filename.db';   -- Vacuum into a new file (non-destructive)
```

---

## Locking, Concurrency, and Blocking Behavior

This section details what happens to concurrent reads and writes during VACUUM - a critical aspect for implementation.

### SQLite's Locking During VACUUM

VACUUM takes an **EXCLUSIVE lock** on the database. From `vacuum.c:264-270`:

```c
/* Begin a transaction and take an exclusive lock on the main database
** file. This is done before the sqlite3BtreeGetPageSize(pMain) call below,
** to ensure that we do not try to change the page-size on a WAL database.
*/
rc = execSql(db, pzErrMsg, "BEGIN");
if( rc!=SQLITE_OK ) goto end_of_vacuum;
rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);  // wrflag=2 means EXCLUSIVE
```

The `sqlite3BtreeBeginTrans(pMain, 2, 0)` call:
- `wrflag=2` means **EXCLUSIVE** (not just write, but exclusive)
- `wrflag=1` would be normal write
- `wrflag=0` would be read

### What Happens to Active Readers?

When VACUUM tries to acquire an exclusive lock:

1. **Readers must finish first** - VACUUM will block (return `SQLITE_BUSY`) until all active read transactions complete
2. **No new readers allowed** - Once VACUUM starts acquiring the lock, new read attempts get `SQLITE_BUSY`
3. **Readers see old data** - Any reader that started before VACUUM will see the pre-VACUUM state

```
Timeline:
─────────────────────────────────────────────────────────────────────
Reader 1:  [======READ======]
Reader 2:       [===READ===]
VACUUM:              [WAITING...][=====EXCLUSIVE VACUUM=====]
Reader 3:                              [BLOCKED - SQLITE_BUSY]
                     ▲
                     VACUUM tries to acquire exclusive lock,
                     waits for Readers 1 & 2 to finish
```

### What Happens to Active Writers?

1. **Cannot have concurrent writers** - SQLite already allows only one writer at a time
2. **Writer must finish first** - If a write transaction is active, VACUUM gets `SQLITE_BUSY`
3. **VACUUM itself cannot run inside a transaction** - This is explicitly checked

### The Complete Lock Acquisition Sequence

```
1. VACUUM command issued
2. Check: Are we in autocommit mode? (If no → error)
3. Check: Are other VDBEs active? (If yes → error)
4. BEGIN transaction on temp database
5. BtreeBeginTrans with wrflag=2 (exclusive) on main database
   └── This blocks until:
       - All active readers finish
       - Any active writer finishes
   └── If busy_timeout expires → SQLITE_BUSY
6. Now VACUUM has exclusive access - no reads or writes possible
7. Copy data to temp DB, copy back
8. Release exclusive lock
```

### Busy Handling

If VACUUM cannot acquire the lock immediately:

```c
// From pager.c - what happens when trying to get exclusive lock
if( pBt->pWriter!=p && (pBt->btsFlags & BTS_EXCLUSIVE)!=0 ){
  sqlite3ConnectionBlocked(p->db, pBt->pWriter->db);
  return SQLITE_LOCKED_SHAREDCACHE;
}
```

VACUUM respects `busy_timeout`:
- If `PRAGMA busy_timeout=N` is set, VACUUM will retry for N milliseconds
- If timeout expires, returns `SQLITE_BUSY`
- Application should handle this and retry later

### WAL Mode Specifics

In WAL mode, locking is different:

1. **Readers don't block writers** (and vice versa) normally
2. **But VACUUM still needs exclusive access** because it rewrites the entire file
3. **VACUUM cannot change page size in WAL mode** - the code explicitly checks:

```c
if( sqlite3PagerGetJournalMode(sqlite3BtreePager(pMain))
                                             ==PAGER_JOURNALMODE_WAL
 && pOut==0
){
  db->nextPagesize = 0;  // Cancel pending page size change
}
```

### WAL Mode Copy-Back Mechanism (Important!)

**SQLite does NOT switch to rollback journal mode for VACUUM.** In WAL mode, the copy-back phase works differently:

1. **Dirty pages go to WAL, not rollback journal**: When `pagerUseWal(pPager) == true`, the pager writes dirty pages to WAL via `pagerWalFrames()` at commit time
2. **No rollback journal needed**: The assertion in `pager.c:6092` confirms this: `assert( pagerUseWal(pPager)==0 )` - rollback journal code only runs when NOT in WAL mode
3. **Checkpoint writes to main DB**: After commit, checkpoint copies WAL frames to the main database file

From `pager.c:6500-6517`:
```c
if( pagerUseWal(pPager) ){
  // WAL MODE: write dirty pages to WAL file
  pList = sqlite3PcacheDirtyList(pPager->pPCache);
  rc = pagerWalFrames(pPager, pList, pPager->dbSize, 1);  // → WAL
}else{
  // ROLLBACK MODE: write to main DB file with journal protection
  ...
}
```

**The comment in vacuum.c about "2x disk space for rollback journal" applies only to rollback mode.**

### VACUUM INTO is Different

`VACUUM INTO 'file.db'` has relaxed locking:

```c
rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);
//                                 ▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲▲
//                                 If pOut!=0 (INTO mode), wrflag=0 (read-only)
```

- Only needs a **read lock** on the source database
- Can run while readers are active
- Cannot run while a writer is active (would get inconsistent snapshot)
- Useful for creating backups without blocking the database

### Implications for Turso DB Implementation

Turso DB has a simpler transaction model (`core/lib.rs:198-211`):

```rust
enum TransactionState {
    Write { schema_did_change: bool },
    Read,
    PendingUpgrade { has_read_txn: bool },
    None,
}
```

For VACUUM implementation:
1. **Must check `auto_commit`** - VACUUM requires autocommit mode
2. **Must check no active transactions** - Return error if `TransactionState != None`
3. **Must acquire exclusive write transaction** - Use `TransactionState::Write`
4. **Must block/error new operations** - While VACUUM runs, reject new queries
5. **VACUUM INTO can use read transaction** - Less restrictive

### Turso Connection/Pager Architecture (Critical for VACUUM)

Each connection creates its **own pager instance** (`core/lib.rs:816`):

```rust
fn _connect(...) -> Result<Arc<Connection>> {
    let pager = if let Some(pager) = pager {
        pager
    } else {
        Arc::new(self._init()?)  // NEW pager instance for each connection
    };
    // ...
}
```

This means:
- `Database.connect()` → new Pager₁ (own file handle, own cache)
- `Database.connect()` → new Pager₂ (own file handle, own cache)
- `Database.connect()` → new Pager₃ (own file handle, own cache)

**Implication for in-place VACUUM**: After copying pages back from temp to main:
- The connection that ran VACUUM sees the new data (same file handle)
- Other connections also see the new data (same underlying file)
- File handles remain valid because we're overwriting the same file, not renaming

**Implication for atomic rename (why it won't work)**:
- After rename, other connections' file handles point to deleted inode
- Those connections would be broken

The database tracks connection count via `n_connections: AtomicUsize`, but this is just a counter - there's no registry to iterate connections.

### Summary Table

| Scenario | Regular VACUUM | VACUUM INTO |
|----------|---------------|-------------|
| Lock type | EXCLUSIVE | READ |
| Blocks readers | Yes (waits for them to finish) | No |
| Blocks writers | Yes | Yes (needs consistent snapshot) |
| Can run during read txn | No - must wait | Yes |
| Can run during write txn | No - must wait | No - must wait |
| Can run inside BEGIN | No - explicit error | No - explicit error |

---

## SQLite's VACUUM Implementation Deep Dive

### High-Level Algorithm

SQLite's VACUUM is implemented in `sqlite/src/vacuum.c` and follows this algorithm:

```
1. Create a new transient (temporary) database file
2. Copy all content from the original database to the transient database
3. Copy content from the transient database back to the original
4. Clean up the transient database
```

This is documented in the source code comment at `vacuum.c:75-104`:

> The transient database requires temporary disk space approximately equal to the size of the original database. The copy operation of step (3) requires additional temporary disk space approximately equal to the size of the original database for the rollback journal. Hence, temporary disk space that is approximately 2x the size of the original database is required.

### Detailed Code Walkthrough

#### Entry Point: `sqlite3Vacuum()` (vacuum.c:105-138)

```c
void sqlite3Vacuum(Parse *pParse, Token *pNm, Expr *pInto){
  Vdbe *v = sqlite3GetVdbe(pParse);
  int iDb = 0;

  // Handle database name (for VACUUM schema_name)
  if( pNm ){
    iDb = sqlite3TwoPartName(pParse, pNm, pNm, &pNm);
    if( iDb<0 ) goto build_vacuum_end;
  }

  // Cannot VACUUM the temp database (iDb==1)
  if( iDb!=1 ){
    int iIntoReg = 0;
    if( pInto && sqlite3ResolveSelfReference(pParse,0,0,pInto,0)==0 ){
      iIntoReg = ++pParse->nMem;
      sqlite3ExprCode(pParse, pInto, iIntoReg);
    }
    // Emit the OP_Vacuum opcode
    sqlite3VdbeAddOp2(v, OP_Vacuum, iDb, iIntoReg);
    sqlite3VdbeUsesBtree(v, iDb);
  }
}
```

**Key Points:**
- VACUUM compiles to a single `OP_Vacuum` bytecode instruction
- Database index `iDb` identifies which database (0=main, 2+=attached)
- Cannot VACUUM the temp database (`iDb==1`)
- `iIntoReg` holds the filename for `VACUUM INTO`

#### Runtime Implementation: `sqlite3RunVacuum()` (vacuum.c:143-427)

This is the core implementation called by the `OP_Vacuum` opcode handler.

##### Phase 1: Precondition Checks (lines 169-188)

```c
if( !db->autoCommit ){
  sqlite3SetString(pzErrMsg, db, "cannot VACUUM from within a transaction");
  return SQLITE_ERROR;
}
if( db->nVdbeActive>1 ){
  sqlite3SetString(pzErrMsg, db, "cannot VACUUM - SQL statements in progress");
  return SQLITE_ERROR;
}
```

**Critical Restrictions:**
- **Cannot VACUUM inside a transaction** - Must be in autocommit mode
- **Cannot VACUUM with other statements active** - Only one VDBE can be running

##### Phase 2: Create and Attach Temp Database (lines 209-232)

```c
// Generate random name for temp database to avoid conflicts
sqlite3_randomness(sizeof(iRandom), &iRandom);
sqlite3_snprintf(sizeof(zDbVacuum), zDbVacuum, "vacuum_%016llx", iRandom);

// Attach the temporary database
rc = execSqlF(db, pzErrMsg, "ATTACH %Q AS %s", zOut, zDbVacuum);
```

**Key Points:**
- Creates a randomly-named attached database (`vacuum_XXXXXX`)
- For `VACUUM INTO`, uses the target filename
- For regular `VACUUM`, uses empty string (creates temp file)

##### Phase 3: Configure Temp Database (lines 260-292)

```c
// Set page size to match original
sqlite3BtreeSetPageSize(pTemp, sqlite3BtreeGetPageSize(pMain), nRes, 0);

// Set auto_vacuum to match (or pending change)
sqlite3BtreeSetAutoVacuum(pTemp, db->nextAutovac>=0 ? db->nextAutovac :
                                         sqlite3BtreeGetAutoVacuum(pMain));
```

**Important Settings Copied:**
- Page size (from `db->nextPagesize` if pending change, otherwise current)
- Reserved bytes at end of each page
- Auto-vacuum mode
- Cache settings

##### Phase 4: Begin Transaction (lines 268-279)

```c
rc = execSql(db, pzErrMsg, "BEGIN");
if( rc!=SQLITE_OK ) goto end_of_vacuum;
rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);
```

**Transaction Handling:**
- Opens a SQL-level transaction on the temp database
- Opens an exclusive Btree-level transaction on the main database
- For `VACUUM INTO`, main database is opened read-only

##### Phase 5: Copy Schema (lines 294-339)

```c
// Force new CREATE statements into vacuum_db
db->init.iDb = nDb;

// Copy table definitions
rc = execSqlF(db, pzErrMsg,
    "SELECT sql FROM \"%w\".sqlite_schema"
    " WHERE type='table' AND name<>'sqlite_sequence'"
    " AND coalesce(rootpage,1)>0",
    zDbMain
);

// Copy index definitions
rc = execSqlF(db, pzErrMsg,
    "SELECT sql FROM \"%w\".sqlite_schema"
    " WHERE type='index'",
    zDbMain
);
```

**Schema Copy Process:**
1. Read each CREATE TABLE statement from original's `sqlite_schema`
2. Execute it in the temp database (creates empty tables)
3. Read each CREATE INDEX statement
4. Execute it in the temp database (creates empty indexes)

**Security Check (lines 43-52):**
```c
// The secondary SQL must be one of CREATE TABLE, CREATE INDEX,
// or INSERT. Historically there have been attacks that first
// corrupt the sqlite_schema.sql field with other kinds of statements
// then run VACUUM to get those statements to execute.
if( zSubSql
 && (strncmp(zSubSql,"CRE",3)==0 || strncmp(zSubSql,"INS",3)==0)
){
  rc = execSql(db, pzErrMsg, zSubSql);
}
```

##### Phase 6: Copy Data (lines 317-326)

```c
rc = execSqlF(db, pzErrMsg,
    "SELECT'INSERT INTO %s.'||quote(name)"
    "||' SELECT*FROM\"%w\".'||quote(name)"
    "FROM %s.sqlite_schema "
    "WHERE type='table' AND coalesce(rootpage,1)>0",
    zDbVacuum, zDbMain, zDbVacuum
);
```

**Data Copy Process:**
- Generates `INSERT INTO vacuum_db.xxx SELECT * FROM main.xxx` for each table
- Uses `quote(name)` to properly escape table names
- Skips virtual tables (rootpage=0)

##### Phase 7: Copy Triggers and Views (lines 333-339)

```c
rc = execSqlF(db, pzErrMsg,
    "INSERT INTO %s.sqlite_schema"
    " SELECT*FROM \"%w\".sqlite_schema"
    " WHERE type IN('view','trigger')"
    " OR(type='table' AND rootpage=0)",
    zDbVacuum, zDbMain
);
```

**Important:** Views, triggers, and virtual tables have no storage - just schema entries.

##### Phase 8: Copy Metadata (lines 348-376)

```c
static const unsigned char aCopy[] = {
   BTREE_SCHEMA_VERSION,     1,  /* Add one to the old schema cookie */
   BTREE_DEFAULT_CACHE_SIZE, 0,  /* Preserve the default page cache size */
   BTREE_TEXT_ENCODING,      0,  /* Preserve the text encoding */
   BTREE_USER_VERSION,       0,  /* Preserve the user version */
   BTREE_APPLICATION_ID,     0,  /* Preserve the application id */
};

for(i=0; i<ArraySize(aCopy); i+=2){
  sqlite3BtreeGetMeta(pMain, aCopy[i], &meta);
  rc = sqlite3BtreeUpdateMeta(pTemp, aCopy[i], meta+aCopy[i+1]);
}
```

**Preserved Metadata:**
- Schema version (cookie) - incremented by 1 to invalidate caches
- Default cache size
- Text encoding (UTF-8, UTF-16LE, UTF-16BE)
- User version (`PRAGMA user_version`)
- Application ID (`PRAGMA application_id`)

##### Phase 9: Copy Back to Original (lines 378-395)

```c
if( pOut==0 ){
  rc = sqlite3BtreeCopyFile(pMain, pTemp);
}
```

For regular VACUUM (not `INTO`), copies the temp database back to the original using `sqlite3BtreeCopyFile()` from `backup.c`.

### The `sqlite3BtreeCopyFile()` Function (backup.c:718-766)

This function performs an efficient page-by-page copy:

```c
int sqlite3BtreeCopyFile(Btree *pTo, Btree *pFrom){
  sqlite3_backup b;

  // Set up backup object
  memset(&b, 0, sizeof(b));
  b.pSrcDb = pFrom->db;
  b.pSrc = pFrom;
  b.pDest = pTo;
  b.iNext = 1;

  // Copy all pages
  sqlite3_backup_step(&b, 0x7FFFFFFF);

  rc = sqlite3_backup_finish(&b);
  if( rc==SQLITE_OK ){
    pTo->pBt->btsFlags &= ~BTS_PAGESIZE_FIXED;
  }
}
```

**Key Implementation Details:**
- Uses the backup API infrastructure for copying
- Copies pages sequentially from page 1
- Handles page size differences (rare)
- Handles the "pending byte page" specially (reserved page at 1GB offset)

### How Page Copy Works (No Raw WAL API)

SQLite does NOT have raw WAL insertion APIs. The `backupOnePage()` function (backup.c:226-279) uses normal pager APIs:

```c
static int backupOnePage(sqlite3_backup *p, Pgno iSrcPg, const u8 *zSrcData, int bUpdate){
  // For each destination page:
  sqlite3PagerGet(pDestPager, iDest, &pDestPg, 0);  // Get dest page
  sqlite3PagerWrite(pDestPg);                        // Mark dirty
  memcpy(zOut, zIn, nCopy);                         // Copy content
  sqlite3PagerUnref(pDestPg);
}
```

At commit time, the pager automatically routes dirty pages to the appropriate destination:
- **WAL mode**: `pagerWalFrames()` writes dirty pages to WAL
- **Rollback mode**: Pages go to main DB file with journal protection

### Why Atomic Rename Doesn't Work

From vacuum.c comments (lines 97-103):
> "Only 1x temporary space and only 1x writes would be required if the copy of step (3) were replaced by **deleting the original database and renaming** the transient database as the original. **But that will not work if other processes are attached** to the original database. And a power loss in between deleting the original and renaming the transient would cause the database file to appear to be deleted following reboot."

**Critical issue for Turso**: Each connection has its own pager with its own file handle. After atomic rename:
- The original file is deleted
- File handles in other connections now point to a non-existent inode
- Those connections would be broken, causing crashes or data corruption

This is why SQLite copies pages BACK to the original file rather than renaming.

### OP_Vacuum Handler (vdbe.c:8098-8103)

```c
case OP_Vacuum: {
  assert( p->readOnly==0 );
  rc = sqlite3RunVacuum(&p->zErrMsg, db, pOp->p1,
                        pOp->p2 ? &aMem[pOp->p2] : 0);
  if( rc ) goto abort_due_to_error;
  break;
}
```

---

## SQLite's Auto-Vacuum and Incremental Vacuum

VACUUM is related to but distinct from auto-vacuum. Understanding both is important.

### Auto-Vacuum Modes

```sql
PRAGMA auto_vacuum = 0;  -- None (default)
PRAGMA auto_vacuum = 1;  -- Full auto-vacuum
PRAGMA auto_vacuum = 2;  -- Incremental
```

### How Auto-Vacuum Works

Auto-vacuum requires additional data structures:

#### Pointer Map Pages

When auto-vacuum is enabled, SQLite maintains "pointer map" (ptrmap) pages:
- Located at specific intervals in the database file
- Each entry is 5 bytes: 1 byte type + 4 bytes parent page number
- Tracks the parent pointer for every page

```c
// From btree.c - Pointer map entry types
#define PTRMAP_ROOTPAGE 1   /* Root page of a b-tree */
#define PTRMAP_FREEPAGE 2   /* Page on the freelist */
#define PTRMAP_OVERFLOW1 3  /* First page of overflow chain */
#define PTRMAP_OVERFLOW2 4  /* Subsequent overflow page */
#define PTRMAP_BTREE 5      /* Non-root b-tree page */
```

#### How Auto-Vacuum Relocates Pages (btree.c:3940-4012)

```c
static int relocatePage(
  BtShared *pBt,
  MemPage *pDbPage,    /* Page to move */
  u8 eType,            /* Pointer map type */
  Pgno iPtrPage,       /* Parent page number */
  Pgno iFreePage,      /* Destination page number */
  int isCommit
){
  // 1. Move page content using pager
  rc = sqlite3PagerMovepage(pPager, pDbPage->pDbPage, iFreePage, isCommit);
  pDbPage->pgno = iFreePage;

  // 2. Update child pointers if this is a b-tree page
  if( eType==PTRMAP_BTREE || eType==PTRMAP_ROOTPAGE ){
    rc = setChildPtrmaps(pDbPage);
  }

  // 3. Update parent's pointer to this page
  if( eType!=PTRMAP_ROOTPAGE ){
    rc = btreeGetPage(pBt, iPtrPage, &pPtrPage, 0);
    rc = modifyPagePointer(pPtrPage, iDbPage, iFreePage, eType);
  }

  // 4. Update pointer map for the new location
  ptrmapPut(pBt, iFreePage, eType, iPtrPage, &rc);
}
```

#### Incremental Vacuum Step (btree.c:4034-4114)

```c
static int incrVacuumStep(BtShared *pBt, Pgno nFin, Pgno iLastPg, int bCommit){
  // Get page type from pointer map
  rc = ptrmapGet(pBt, iLastPg, &eType, &iPtrPage);

  if( eType==PTRMAP_FREEPAGE ){
    // Page is on freelist - remove it
    rc = allocateBtreePage(pBt, &pFreePg, &iFreePg, iLastPg, BTALLOC_EXACT);
  } else {
    // Page has content - relocate it to a lower page number
    rc = allocateBtreePage(pBt, &pFreePg, &iFreePg, iNear, eMode);
    rc = relocatePage(pBt, pLastPg, eType, iPtrPage, iFreePg, bCommit);
  }
}
```

### VACUUM vs Auto-Vacuum Comparison

| Aspect | VACUUM | Auto-Vacuum |
|--------|--------|-------------|
| When | Manual command | Automatic on commit |
| Space Required | 2x database size | None extra |
| Reorganization | Complete rebuild | Incremental |
| Indexes | Fully rebuilt | Not rebuilt |
| Schema Version | Incremented | Unchanged |
| WAL Mode | Cannot change page size | Works normally |

---

## Turso DB Current Architecture

### Relevant Components

#### Parser (`parser/src/ast.rs:284-289`)

VACUUM is already parsed:

```rust
pub enum Stmt {
    // ...
    /// `VACUUM`: database name, into expr
    Vacuum {
        /// database name
        name: Option<Name>,
        /// into expression
        into: Option<Box<Expr>>,
    },
    // ...
}
```

#### Translation (`core/translate/mod.rs:312`)

Currently returns an error:

```rust
ast::Stmt::Vacuum { .. } => bail_parse_error!("VACUUM not supported yet"),
```

#### Storage Layer

**Pager** (`core/storage/pager.rs`):
- Manages page cache, reads, writes
- Has freelist management: `free_page()`, `allocate_page()`
- Has `AutoVacuumMode` enum: `None`, `Full`, `Incremental`
- Has ptrmap support via `ptrmap_get()`, `ptrmap_put()`

**Freelist** (`core/storage/pager.rs:3761-3893`):
- Trunk pages contain pointers to leaf pages
- `free_page()` adds page to freelist
- `allocate_page()` takes from freelist or extends file

**Ptrmap Module** (`core/storage/pager.rs:4492-4627`):
- `PtrmapType` enum: `RootPage`, `FreePage`, `Overflow1`, `Overflow2`, `BTreeNode`
- `is_ptrmap_page()` - check if page number is a ptrmap page
- `get_ptrmap_page_no_for_db_page()` - find ptrmap page for a given data page

#### B-Tree Operations (`core/storage/btree.rs`)

- `BTreeCursor` - cursor for traversing B-trees
- Destroy operations exist via `btree_destroy()`

#### ATTACH/DETACH (`core/translate/attach.rs`)

Already implemented:
- `translate_attach()` - creates bytecode for ATTACH
- `translate_detach()` - creates bytecode for DETACH
- Uses `ScalarFunc::Attach` and `ScalarFunc::Detach`

---

## ATTACH Database Support

### Does Turso DB Support ATTACH?

**Yes, but with limitations.** Turso DB has ATTACH/DETACH implemented:

**What's Implemented** (`core/connection.rs:1150-1231`):
```rust
pub fn attach_database(&self, path: &str, alias: &str) -> Result<()> {
    // ...
    // FIXME: for now, only support read only attach
    let main_db_flags = self.db.open_flags | OpenFlags::ReadOnly;
    let db = Self::from_uri_attached(path, db_opts, main_db_flags, io)?;
    // ...
}
```

**Current Limitations:**
1. **Read-only only** - Comment says "FIXME: for now, only support read only attach"
2. No write operations to attached databases
3. Full schema resolution for attached databases exists

### Do We Need VACUUM for Attached Databases?

**Yes, `VACUUM schema_name` should work for attached databases.**

SQLite behavior:
- `VACUUM` - vacuums main database only
- `VACUUM main` - same as above
- `VACUUM schema_name` - vacuums specific attached database
- Cannot vacuum `temp` database (always error)

### Implications for VACUUM Implementation

**Phase 1: Main database only**
- Implement `VACUUM` and `VACUUM main`
- Reject `VACUUM schema_name` for attached databases (for now)

**Phase 2: Attached database support**
- Requires write support for attached databases first
- Need to resolve correct pager for the target database
- Use `connection.resolve_database_id()` to get database index

**Code Path for Attached Databases:**
```rust
// From core/connection.rs:1248-1276
pub(crate) fn resolve_database_id(&self, qualified_name: &ast::QualifiedName) -> Result<usize> {
    if let Some(db_name) = &qualified_name.db_name {
        match_ignore_ascii_case!(match name_bytes {
            b"main" => Ok(0),
            b"temp" => Ok(1),  // VACUUM should reject this
            _ => {
                // Look up attached database
                if let Some((idx, _attached_db)) = self.get_attached_database(&db_name_normalized) {
                    Ok(idx)
                } else {
                    Err(LimboError::InvalidArgument(...))
                }
            }
        })
    } else {
        Ok(0)  // Default to main
    }
}
```

### VACUUM Uses ATTACH Internally

SQLite's VACUUM internally uses ATTACH to create the temp database:

```c
// From vacuum.c:223-226
rc = execSqlF(db, pzErrMsg, "ATTACH %Q AS %s", zOut, zDbVacuum);
```

For Turso DB implementation:
1. **Option A**: Use existing ATTACH infrastructure (needs write support)
2. **Option B**: Create temp database directly without ATTACH
3. **Option C**: Use in-memory database as temp (avoids ATTACH entirely)

**Recommendation:** Option C for initial implementation - simpler and avoids circular dependency on ATTACH write support.

---

## Implementation Plan

### Approach Selection

SQLite uses the "create new, copy, replace" approach. For Turso DB, there are two options:

**Option A: In-Place Vacuum (Simpler)**
- Iterate through all pages
- Build mapping of used vs unused pages
- Relocate pages from high addresses to low addresses
- Truncate file
- Similar to auto-vacuum but complete

**Option B: Temp Database Approach (SQLite-compatible)**
- Create temporary database (file or in-memory)
- Copy all data via SQL
- Replace original
- More disk space but simpler logic

**Recommendation:** Start with Option B for maximum SQLite compatibility, then optimize later.

### Implementation Phases

#### Phase 1: Basic VACUUM (Temp Database Approach)

##### Step 1.1: Add VDBE Instruction (`core/vdbe/insn.rs`)

```rust
/// Vacuum the database, rebuilding it to reclaim space.
Vacuum {
    /// Database index (0 = main, 2+ = attached)
    db_idx: usize,
    /// Register containing filename for VACUUM INTO (0 if none)
    into_reg: usize,
},
```

##### Step 1.2: Add Translation (`core/translate/vacuum.rs`)

Create new file:

```rust
pub fn translate_vacuum(
    name: &Option<Name>,
    into: &Option<Box<Expr>>,
    resolver: &Resolver,
    mut program: ProgramBuilder,
    connection: &Connection,
) -> Result<ProgramBuilder> {
    // 1. Check we're in autocommit mode
    // 2. Determine database index from name
    // 3. Handle INTO expression if present
    // 4. Emit Vacuum instruction
    program.emit_insn(Insn::Vacuum {
        db_idx: 0,
        into_reg,
    });
    Ok(program)
}
```

##### Step 1.3: Add Execution Handler (`core/vdbe/execute.rs`)

```rust
pub struct OpVacuumState {
    phase: VacuumPhase,
    temp_db_name: String,
    tables_to_copy: Vec<TableInfo>,
    current_table_idx: usize,
}

enum VacuumPhase {
    CheckPreconditions,
    AttachTempDb,
    CopySchema,
    CopyData { table_idx: usize },
    CopyMetadata,
    CopyBack,
    DetachTempDb,
    Done,
}

pub fn op_vacuum(
    pager: &Arc<Pager>,
    connection: &Connection,
    db_idx: usize,
    into_reg: usize,
    state: &mut OpVacuumState,
) -> Result<InsnFunctionStepResult> {
    loop {
        match state.phase {
            VacuumPhase::CheckPreconditions => {
                // Verify autocommit mode
                // Verify no other statements active
                state.phase = VacuumPhase::AttachTempDb;
            }
            VacuumPhase::AttachTempDb => {
                // Generate random temp db name
                // Execute ATTACH '' AS vacuum_xxxxx
                state.phase = VacuumPhase::CopySchema;
            }
            // ... other phases
        }
    }
}
```

##### Step 1.4: Wire Up Translation (`core/translate/mod.rs`)

```rust
ast::Stmt::Vacuum { name, into } => {
    translate_vacuum(name, into, &resolver, program, connection)
}
```

#### Phase 2: VACUUM INTO Support

- Store target filename in register
- Skip copy-back phase
- Don't delete temp database (it IS the result)

#### Phase 3: Named Database Support

- Parse schema name
- Validate it exists and is not temp
- Use correct pager/btree for the target database

#### Phase 4: Optimization - In-Place Vacuum

For databases without auto-vacuum, implement direct page relocation:

1. Scan all B-trees to find used pages
2. Build page relocation map
3. Relocate pages from high to low addresses
4. Update root page numbers in schema
5. Truncate file

---

## Edge Cases and Gotchas

### From SQLite's Implementation

#### 1. Cannot VACUUM Inside Transaction

```c
if( !db->autoCommit ){
  sqlite3SetString(pzErrMsg, db, "cannot VACUUM from within a transaction");
  return SQLITE_ERROR;
}
```

**Why:** VACUUM needs to modify the entire database file atomically. A partial VACUUM in a transaction that rolls back would leave corruption.

#### 2. Cannot VACUUM With Active Statements

```c
if( db->nVdbeActive>1 ){
  sqlite3SetString(pzErrMsg, db, "cannot VACUUM - SQL statements in progress");
  return SQLITE_ERROR;
}
```

**Why:** Other cursors might hold references to pages that VACUUM needs to relocate.

#### 3. Cannot VACUUM Temp Database

```c
if( iDb!=1 ){
  // ... emit OP_Vacuum
}
```

**Why:** Temp database is session-local and typically memory-backed. Vacuuming it makes no sense.

#### 4. WAL Mode Page Size Restriction

```c
if( sqlite3PagerGetJournalMode(sqlite3BtreePager(pMain))
                                             ==PAGER_JOURNALMODE_WAL
 && pOut==0
){
  db->nextPagesize = 0;  // Disable pending page size change
}
```

**Why:** Cannot change page size of a WAL-mode database without switching journal modes first.

#### 5. VACUUM INTO Target Must Not Exist

```c
if( id->pMethods!=0 && (sqlite3OsFileSize(id, &sz)!=SQLITE_OK || sz>0) ){
  rc = SQLITE_ERROR;
  sqlite3SetString(pzErrMsg, db, "output file already exists");
  goto end_of_vacuum;
}
```

**Why:** Prevents accidental data loss and ensures clean state.

#### 6. Schema SQL Security Check

```c
if( zSubSql
 && (strncmp(zSubSql,"CRE",3)==0 || strncmp(zSubSql,"INS",3)==0)
){
  rc = execSql(db, pzErrMsg, zSubSql);
}
```

**Why:** Historically, attackers corrupted `sqlite_schema.sql` with malicious statements, then ran VACUUM to execute them.

#### 7. Virtual Tables Have No Storage

```c
// Copy table definitions (skip virtual tables - rootpage=0)
"WHERE type='table' AND name<>'sqlite_sequence'"
" AND coalesce(rootpage,1)>0"
```

Virtual tables store only schema, no data pages.

#### 8. Disk Space Requirements

**Rollback mode**: VACUUM requires approximately 2x database size:
- 1x for the temp database
- 1x for the rollback journal when copying back

**WAL mode** (Turso): VACUUM requires approximately 1-2x database size:
- 1x for the temp database
- WAL file grows during copy-back (dirty pages go to WAL)
- After checkpoint, WAL can be truncated
- No rollback journal needed

#### 9. Schema Version Increment

```c
BTREE_SCHEMA_VERSION, 1,  /* Add one to the old schema cookie */
```

Incrementing schema version invalidates prepared statements in other connections.

#### 10. Memory Databases

```c
isMemDb = sqlite3PagerIsMemdb(sqlite3BtreePager(pMain));
```

Special handling needed for `:memory:` databases - they can still be vacuumed but behavior differs.

#### 11. AUTOINCREMENT Tables (sqlite_sequence)

From `vacuum.test:352-384`:
```sql
CREATE TABLE autoinc(a INTEGER PRIMARY KEY AUTOINCREMENT, b);
INSERT INTO autoinc(b) VALUES('hi');
DELETE FROM autoinc;
VACUUM;
INSERT INTO autoinc(b) VALUES('one');  -- Should continue sequence, not restart
```

The `sqlite_sequence` table must be preserved during VACUUM to maintain AUTOINCREMENT counters.

#### 12. Special Characters in Table Names

From `vacuum.test:263-274`:
```sql
CREATE TABLE "abc abc"(a, b, c);  -- Table with space in name
INSERT INTO "abc abc" VALUES(1, 2, 3);
VACUUM;
```

Must properly quote table names when generating internal SQL.

#### 13. Database with Single Quote in Path

From `vacuum.test:337-348`:
```sql
-- File: a'z.db
CREATE TABLE t1(t);
VACUUM;
```

Paths with special characters must be properly escaped.

#### 14. BLOB Data Preservation

From `vacuum.test:277-290`:
```sql
INSERT INTO t1 VALUES(X'00112233', NULL, NULL);
VACUUM;
SELECT count(*) FROM t1 WHERE a = X'00112233';  -- Must return 1
```

Binary data must survive VACUUM without corruption.

#### 15. Views and Triggers Have No Root Page

From `vacuum.c:333-339`:
```sql
-- Views and triggers have rootpage=0 in sqlite_schema
INSERT INTO vacuum_db.sqlite_schema
SELECT * FROM main.sqlite_schema
WHERE type IN('view','trigger')
   OR (type='table' AND rootpage=0)  -- Virtual tables
```

These are schema-only entries with no B-tree data.

#### 16. Schema Cookie Must Be Incremented

From `vacuum.test:154-181` - Schema version increment invalidates cached statements:
```
Before VACUUM: schema_version = N
After VACUUM:  schema_version = N+1
```

Other connections will re-parse their prepared statements.

#### 17. Generated Columns with CHECK Constraints

From `vacuum-into.test:29-57`:
```sql
CREATE TABLE t1(
    a INTEGER PRIMARY KEY,
    b ANY,
    c INT AS (b+1),                          -- Generated column
    CHECK( typeof(b)!='integer' OR b>a-5 )   -- CHECK constraint
);
VACUUM INTO 'out.db';
```

This is tricky because CHECK constraints are ignored for read-only databases, but the xfer optimization needs matching schemas.

#### 18. Freelist Must Be Reconstructed

After VACUUM, freelist should be empty (all pages compacted):
```sql
PRAGMA freelist_count;  -- Should be 0 after VACUUM
```

From `vacuum.test:309-320`.

#### 19. VACUUM Works Even with App-Defined Functions

From `vacuum.test:62-68`:
```sql
-- Even if user overrides substr, like, quote with failing functions
db func substr failing_app_func
db func like failing_app_func
VACUUM;  -- Must still work, using built-in functions
```

VACUUM must use built-in functions, not user-defined overrides.

#### 20. VACUUM with ATTACH Disabled

From `vacuum.test:406-423`:
```sql
sqlite3_db_config db ATTACH_CREATE 0
VACUUM;  -- Must still work
```

VACUUM should work even if ATTACH is disabled, using internal mechanisms.

### Common Failure Modes

#### Failure 1: Disk Full During VACUUM

If disk fills up during VACUUM:
- Original database must remain intact
- Temp database should be cleaned up
- Error must be returned to user

#### Failure 2: Power Loss/Crash During VACUUM

Critical moments:
1. **During temp DB creation** - No problem, temp DB is incomplete
2. **During copy-back phase** - This is protected by transaction on main DB
3. **During finalization** - File might be larger than needed but valid

SQLite's design ensures the main database is always consistent.

#### Failure 3: Out of Memory

VACUUM should not load entire database into memory:
- Process tables one at a time
- Use streaming/cursor-based copying
- Page cache limits apply

### Turso-Specific Considerations

#### 1. Async I/O Pattern

Turso uses state machines for async I/O. VACUUM will need complex state machine with many phases:

```rust
enum VacuumState {
    CheckPreconditions,
    AcquireExclusiveLock,
    CreateTempDatabase,
    CopySchemaTable { table_idx: usize },
    CopySchemaIndex { index_idx: usize },
    CopyTableData { table_idx: usize, cursor: ... },
    CopyViewsAndTriggers,
    CopyMetadata,
    CopyBackToMain { page_idx: usize },
    TruncateMainDb,
    CleanupTempDb,
    Done,
}
```

Each state may yield for I/O and resume later.

#### 2. MVCC Support - Can We VACUUM Concurrently?

**Short answer: No concurrent VACUUM, but VACUUM *with* MVCC is possible with proper coordination.**

Turso's MVCC uses a **dual-cursor architecture** (`core/mvcc/cursor.rs`):
```rust
/// We read rows from MVCC index or BTree in a dual-cursor approach.
/// This means we read rows from both cursors and then advance the cursor that was just consumed.
struct DualCursorPeek {
    mvcc_peek: CursorPeek,   // Next row from MVCC in-memory store
    btree_peek: CursorPeek,  // Next row from B-tree on disk
}
```

The MVCC store structure (`core/mvcc/database/mod.rs`):
```rust
pub struct RowID {
    /// The table ID. Analogous to table's root page number.
    pub table_id: MVTableId,  // Note: "analogous to" - likely a logical ID, not literal root page
    pub row_id: RowKey,
}

pub struct RowVersion {
    // ...
    /// Flag indicating if row existed in B-tree before MVCC was enabled
    pub btree_resident: bool,
}
```

**Why Concurrent VACUUM Doesn't Work:**

1. **Dual-cursor model breaks during active reads**
   - MVCC cursor reads from BOTH B-tree AND in-memory store
   - If VACUUM rewrites B-tree while cursor iterates → inconsistent results
   - Active readers would see corrupted/mixed data

2. **Checkpoint conflicts**
   - MVCC checkpoint flushes versions to B-tree periodically
   - If VACUUM runs during checkpoint → corruption
   - From docs: "Checkpoint blocks other transactions, even reads!"

**Why MVCC is NOT Fundamentally Incompatible:**

The key insight is that `table_id` is described as "**analogous to**" root page number - suggesting it's likely a **logical identifier** that maps to a table name, not literally the physical root page. If so, it survives VACUUM since table names don't change.

VACUUM can work with MVCC if properly coordinated:

1. **VACUUM requires exclusive access anyway** - No active readers/writers
2. **Force MVCC checkpoint before VACUUM** - Flush all in-memory versions to B-tree
3. **Clear the MVCC in-memory store** - Nothing left to merge
4. **Run VACUUM normally** - B-tree is rewritten
5. **MVCC restarts fresh** - New transactions work against vacuumed B-tree

**Remaining Concerns:**

1. **The `.db-log` file**
   - MVCC stores persistent version data in this file
   - After VACUUM rewrites B-tree, does the log reference invalid locations?
   - May need to invalidate/clear the log file as part of VACUUM

2. **Recovery semantics**
   - If crash happens after VACUUM but before log cleanup, what happens?
   - Need to ensure atomic transition

3. **`btree_resident` flag ambiguity**
   - Flag tracks if row was in B-tree before MVCC was enabled
   - After VACUUM, all surviving rows are in the "new" B-tree
   - Checkpoint logic may need adjustment

**Implementation Decision:**

```rust
// In op_vacuum():
if connection.mv_store().is_some() {
    // Phase 1: Block VACUUM (safe, simple)
    return Err(LimboError::NotSupported(
        "VACUUM not supported with MVCC enabled. Disable MVCC first."
    ));

    // Phase 2: Proper MVCC coordination
    // 1. Wait for all active MVCC transactions to complete
    // 2. Force MVCC checkpoint (flush to B-tree)
    // 3. Clear/invalidate .db-log file
    // 4. Clear in-memory MVCC store
    // 5. Run VACUUM
    // 6. MVCC can resume with fresh state
}
```

**Recommendation:**
- **Phase 1:** Return error if MVCC is enabled (safe, simple)
- **Phase 2:** Implement proper coordination:
  1. Force MVCC checkpoint (flush all versions to B-tree)
  2. Clear/invalidate the `.db-log` file
  3. Clear the in-memory MVCC store
  4. Run VACUUM as normal
  5. MVCC resumes with clean state

This is operationally complex but not fundamentally impossible.

#### 3. Auto-Vacuum Interaction

If database has `auto_vacuum != None`:
- **Full auto-vacuum**: VACUUM may be a no-op (already compacted)
- **Incremental**: VACUUM should fully compact, then re-enable incremental
- Ptrmap pages must be preserved or regenerated
- `vacuum_mode_largest_root_page` header field must be updated

Current Turso code (`core/lib.rs:652-663`):
```rust
let is_autovacuumed_db = self.io.block(|| {
    pager.with_header(|header| {
        header.vacuum_mode_largest_root_page.get() > 0
            || header.incremental_vacuum_enabled.get() > 0
    })
})?;
```

#### 4. Sync Mode

VACUUM should respect sync mode but ensure durability:
- `PRAGMA synchronous=OFF`: Still sync at critical points during VACUUM
- Temp database can use OFF (it's transient)
- Main database copy-back must use configured sync mode

#### 5. Encryption

If database is encrypted (`connection.encryption_key`):
- Temp database must use same encryption key and cipher
- VACUUM INTO must also encrypt the target
- Key derivation must be consistent

#### 6. Connection State

Check connection state before VACUUM:
```rust
// Must verify:
connection.auto_commit == true
connection.transaction_state == TransactionState::None
connection.query_only == false
!connection.is_closed()
```

#### 7. Attached Databases Catalog

Turso has `attached_databases: RwLock<DatabaseCatalog>`:
- VACUUM must operate on correct pager
- Must not corrupt catalog during operation
- Schema resolution must work for `VACUUM schema_name`

#### 8. Page Cache Invalidation

After VACUUM:
- All cached pages are invalid
- Must call `page_cache.clear()` or similar
- Other connections' caches must be invalidated (via schema cookie bump)

#### 9. Incremental View Maintenance (IVM / DBSP) - Critical Interaction

**IVM directly references B-tree pages and will break during VACUUM.**

##### How IVM Works

IVM (Incremental View Maintenance) uses DBSP (Database Stream Processing) circuits to maintain materialized views incrementally. Key components:

1. **Hidden State Tables** (`core/schema.rs:151`):
   ```rust
   pub const DBSP_TABLE_PREFIX: &str = "__turso_internal_dbsp_state_v";
   // Full name: __turso_internal_dbsp_state_v<version>_<viewname>
   ```

2. **Persistent B-tree Storage** (`core/incremental/persistence.rs`):
   ```rust
   use crate::storage::btree::{BTreeCursor, BTreeKey, CursorTrait};

   pub struct DbspStateCursors {
       pub table_cursor: BTreeCursor,   // B-tree cursor for state table
       pub index_cursor: BTreeCursor,   // B-tree cursor for state index
   }
   ```

3. **Root Page References** (`core/incremental/view.rs:223`):
   ```rust
   pub struct IncrementalView {
       name: String,
       // Root page of the btree storing the materialized state
       root_page: i64,  // ← Direct reference to B-tree root page!
       // ...
   }
   ```

##### Why VACUUM Breaks IVM

| Component | Problem |
|-----------|---------|
| `IncrementalView.root_page` | Caches root page number. VACUUM changes root pages. |
| `DbspStateCursors` | Hold open B-tree cursors. VACUUM rewrites pages under them. |
| Schema cache | Stores `(table_root, index_root)` for DBSP tables. Becomes stale. |
| `ViewTransactionState` | In-memory deltas reference table structure. |

##### IVM Storage Architecture

```
IncrementalView "my_view"
    │
    ├── root_page: 42  ←──────────────────────────────┐
    │                                                  │
    ├── circuit: DbspCircuit                           │ References
    │                                                  │
    └── tracker: ComputationTracker                    │
                                                       ▼
Database File:                               ┌─────────────────────┐
  Page 1: Header                             │ __turso_internal_   │
  Page 2: sqlite_schema                      │ dbsp_state_v1_      │
  ...                                        │ my_view             │
  Page 42: DBSP state table root ────────────┤ (B-tree table)      │
  Page 43: DBSP state table data             └─────────────────────┘
  Page 44: DBSP state index root
  ...

After VACUUM:
  Page 42 might now be something else!
  IncrementalView.root_page still says 42 → CORRUPT READ
```

##### What VACUUM Must Do for IVM

**Option A: Block VACUUM if IVM is active**
```rust
// In op_vacuum():
if schema.has_any_incremental_views() {
    return Err(LimboError::NotSupported(
        "VACUUM not supported with incremental views"
    ));
}
```

**Option B: Properly handle IVM during VACUUM**
1. **Flush all pending deltas** - Commit any in-memory changes
2. **Close all DBSP cursors** - Release B-tree cursor references
3. **VACUUM normally** - Rebuild all tables including DBSP state tables
4. **Update root page references** - After VACUUM:
   ```rust
   // Re-read root pages from sqlite_schema for DBSP tables
   for view in schema.incremental_views.values_mut() {
       let new_root = lookup_dbsp_table_root(&view.name)?;
       view.root_page = new_root;
   }
   ```
5. **Invalidate caches** - Clear schema caches, reopen cursors

**Option C: Treat DBSP tables as regular tables (Simplest)**
- VACUUM already copies ALL tables including `__turso_internal_*` tables
- The issue is just updating cached root page references
- After VACUUM, bump schema version → force schema re-parse
- Schema re-parse will read new root pages from sqlite_schema

##### IVM vs MVCC Comparison

| Aspect | MVCC | IVM |
|--------|------|-----|
| **References B-tree pages?** | Indirectly (table_id ≈ root page) | Directly (root_page field) |
| **In-memory state?** | Version chains in SkipMap | Deltas in ViewTransactionState |
| **Persistent storage?** | `.db-log` file | Hidden B-tree tables in main DB |
| **Dual-cursor model?** | Yes (MVCC + B-tree merged) | No (just B-tree cursors) |
| **VACUUM feasibility** | Very hard | Manageable with cache invalidation |

##### Why IVM is Easier Than MVCC for VACUUM

**Good news: IVM is actually manageable**, unlike MVCC. Key differences:

1. **DBSP tables are regular B-trees**
   - Stored in main database file as normal tables
   - VACUUM copies them like any other table
   - No special handling needed during the copy phase

2. **No dual-cursor problem**
   - MVCC merges in-memory versions with B-tree data at read time
   - IVM just uses standard B-tree cursors
   - No risk of inconsistent merge during VACUUM

3. **Schema cookie mechanism helps**
   - After VACUUM, schema cookie is bumped
   - All connections re-parse schema
   - Re-parsing reads NEW root pages from sqlite_schema
   - `IncrementalView` objects are reconstructed with correct root pages

4. **In-memory deltas are transient**
   - `ViewTransactionState` holds uncommitted changes
   - These are either committed (flushed to B-tree) or rolled back
   - VACUUM runs in autocommit mode, so no pending deltas

**The key insight**: If we ensure VACUUM bumps the schema cookie (which SQLite always does), the existing schema invalidation mechanism handles IVM automatically. The only risk is if `IncrementalView.root_page` is cached somewhere that survives schema re-parse.

##### Recommendation for IVM

**For Phase 1**: Check if incremental views exist and return error:
```rust
if !schema.incremental_views.is_empty() {
    return Err(LimboError::NotSupported(
        "VACUUM not supported with incremental views. Drop views first."
    ));
}
```

**For Phase 2**: Implement proper handling:
1. Schema version bump already invalidates prepared statements
2. After VACUUM, force schema re-parse (already happens via cookie bump)
3. Incremental views will re-initialize with correct root pages on next use
4. Key: Make sure `IncrementalView.root_page` is re-read from schema, not cached

#### 10. Index Methods (FTS, Vector)

Custom index methods (`core/index_method/`):
- FTS indexes have special storage
- Vector indexes may have external files
- Must be rebuilt or migrated properly

#### Summary: Pre-VACUUM Checklist for Turso DB

Before running VACUUM, check these conditions:

```rust
fn can_vacuum(connection: &Connection, schema: &Schema) -> Result<()> {
    // 1. Must be in autocommit mode
    if !connection.auto_commit.load(Ordering::SeqCst) {
        return Err("cannot VACUUM from within a transaction");
    }

    // 2. Must not have active transaction
    if connection.get_tx_state() != TransactionState::None {
        return Err("cannot VACUUM - transaction in progress");
    }

    // 3. Must not be in query-only mode
    if connection.query_only.load(Ordering::SeqCst) {
        return Err("cannot VACUUM - connection is query-only");
    }

    // 4. Check MVCC - Phase 1: block, Phase 2: coordinate
    if connection.mv_store().is_some() {
        // Phase 1: Simple blocking
        return Err("cannot VACUUM with MVCC enabled");
        // Phase 2: Force checkpoint, clear store, then proceed
    }

    // 5. Check IVM (Phase 1: block, Phase 2: handle properly)
    if !schema.incremental_views.is_empty() {
        return Err("cannot VACUUM with incremental views");
    }

    // 6. Cannot vacuum temp database
    if target_db_index == 1 {
        return Err("cannot VACUUM temp database");
    }

    // 7. VACUUM INTO target must not exist
    if is_vacuum_into && target_file_exists {
        return Err("output file already exists");
    }

    Ok(())
}
```

**Post-VACUUM actions:**
1. Bump schema cookie (invalidates prepared statements)
2. Clear page cache
3. Force schema re-parse on all connections
4. IVM: Root pages automatically updated via schema re-parse
5. Verify `PRAGMA integrity_check` passes

---

## Testing Strategy

### Unit Tests

1. **Basic VACUUM** - Verify file shrinks after deleting data
2. **VACUUM INTO** - Verify new file created correctly
3. **Data Integrity** - All data survives VACUUM
4. **Index Integrity** - All indexes work after VACUUM
5. **Transaction Restriction** - Error when in transaction
6. **Active Statement Restriction** - Error with other statements

### Integration Tests

1. **Large Database** - VACUUM database with many tables/rows
2. **Concurrent Access** - Other connections blocked during VACUUM
3. **Crash Recovery** - Database recoverable if crash during VACUUM
4. **WAL Mode** - VACUUM works correctly in WAL mode
5. **Auto-Vacuum Interaction** - Correct behavior with auto_vacuum enabled

### Compatibility Tests

Use SQLite's TCL test suite:
- `vacuum.test` - Main VACUUM tests
- `vacuum2.test` - Additional VACUUM tests
- `vacuum3.test` - VACUUM with attached databases
- `vacuum4.test` - VACUUM with corrupt databases
- `incrvacuum.test` - Incremental vacuum tests

### Performance Tests

1. **Time vs Database Size** - Linear scaling expected
2. **Memory Usage** - Should not load entire DB into memory
3. **Disk I/O** - Approximately 3x database size in I/O

---

## References

### SQLite Source Files

- `sqlite/src/vacuum.c` - Main VACUUM implementation
- `sqlite/src/backup.c` - Backup API used by VACUUM (contains `sqlite3BtreeCopyFile`)
- `sqlite/src/btree.c` - B-tree operations, auto-vacuum, incremental vacuum
- `sqlite/src/vdbe.c` - OP_Vacuum handler
- `sqlite/tool/fast_vacuum.c` - Alternative VACUUM implementation (demo)

### Turso DB Source Files

- `parser/src/ast.rs` - VACUUM AST definition
- `parser/src/parser.rs` - VACUUM parsing (`parse_vacuum()`)
- `core/translate/mod.rs` - Translation dispatch (add VACUUM case)
- `core/translate/attach.rs` - ATTACH/DETACH implementation (pattern to follow)
- `core/vdbe/insn.rs` - Instruction definitions
- `core/vdbe/execute.rs` - Instruction execution
- `core/storage/pager.rs` - Pager, freelist, ptrmap
- `core/storage/btree.rs` - B-tree operations

### SQLite Documentation

- https://sqlite.org/lang_vacuum.html - VACUUM command documentation
- https://sqlite.org/pragma.html#pragma_auto_vacuum - Auto-vacuum pragma
- https://sqlite.org/fileformat.html - File format details

---

## Appendix: Complete B-tree Dependencies in Turso DB

This section documents ALL structures in Turso DB that reference B-tree pages or cursors, which VACUUM must account for.

### 1. Schema Layer (`core/schema.rs`)

#### 1.1 BTreeTable Structure (lines 1524-1534)
```rust
pub struct BTreeTable {
    pub root_page: i64,  // ← Direct B-tree root page reference!
    pub name: String,
    pub primary_key_columns: Vec<(String, SortOrder)>,
    pub columns: Vec<Column>,
    pub has_rowid: bool,
    pub is_strict: bool,
    pub has_autoincrement: bool,
    pub unique_sets: Vec<UniqueSet>,
    pub foreign_keys: Vec<Arc<ForeignKey>>,
}
```

**VACUUM Impact:** Every `BTreeTable` caches its root page. After VACUUM, these are stale.

#### 1.2 Index Structure (lines 2616-2631)
```rust
pub struct Index {
    pub name: String,
    pub table_name: String,
    pub root_page: i64,  // ← Direct B-tree root page reference!
    pub columns: Vec<IndexColumn>,
    pub unique: bool,
    pub ephemeral: bool,
    pub has_rowid: bool,
    pub where_clause: Option<Box<Expr>>,
    pub index_method: Option<Arc<dyn IndexMethodAttachment>>,
}
```

**VACUUM Impact:** Every `Index` caches its root page. After VACUUM, these are stale.

#### 1.3 Schema Structure (lines 186-213)
```rust
pub struct Schema {
    pub tables: HashMap<String, Arc<Table>>,     // Contains BTreeTable with root_page
    pub indexes: HashMap<String, VecDeque<Arc<Index>>>,  // Contains Index with root_page
    pub incremental_views: HashMap<String, Arc<Mutex<IncrementalView>>>,  // Contains root_page
    pub analyze_stats: AnalyzeStats,  // Statistics keyed by table/index name
    pub schema_version: u32,
    // ... other fields
}
```

**VACUUM Impact:** The entire schema cache becomes stale. Schema cookie bump forces re-parse.

### 2. Cursor Types (`core/types.rs`, `core/vdbe/builder.rs`)

#### 2.1 Main Cursor Enum (types.rs:2636-2643)
```rust
pub enum Cursor {
    BTree(Box<dyn CursorTrait>),                          // B-tree cursor
    IndexMethod(Box<dyn IndexMethodCursor>),              // May wrap B-tree
    Pseudo(Box<PseudoCursor>),                            // No B-tree
    Sorter(Box<Sorter>),                                  // Ephemeral, no B-tree
    Virtual(VirtualTableCursor),                          // May have shadow B-trees
    MaterializedView(Box<MaterializedViewCursor>),        // Wraps B-tree cursor
}
```

#### 2.2 CursorType in Programs (builder.rs:198-209)
```rust
pub enum CursorType {
    BTreeTable(Arc<BTreeTable>),          // References BTreeTable with root_page
    BTreeIndex(Arc<Index>),               // References Index with root_page
    IndexMethod(Arc<dyn IndexMethodAttachment>),
    Pseudo(PseudoCursorType),
    Sorter,
    VirtualTable(Arc<VirtualTable>),
    MaterializedView(Arc<BTreeTable>, Arc<Mutex<IncrementalView>>),  // Both have root_page
}
```

**VACUUM Impact:** Active cursors in VDBE programs hold references to BTreeTable/Index. These programs must be invalidated or completed before VACUUM.

### 3. VDBE Program State (`core/vdbe/mod.rs`)

#### 3.1 Program Structure (lines 795-820)
```rust
pub struct Program {
    pub cursor_ref: Vec<(Option<CursorKey>, CursorType)>,  // CursorType has root_page refs
    pub table_references: TableReferences,                  // References to Arc<BTreeTable>
    pub connection: Arc<Connection>,
    // ...
}
```

#### 3.2 ProgramState (lines 340-355)
```rust
pub struct ProgramState {
    pub(crate) cursors: Vec<Option<Cursor>>,  // Active cursor instances
    // ...
}
```

**VACUUM Impact:** Running programs have open cursors. VACUUM requires all programs to complete first.

### 4. Incremental View Maintenance (IVM / DBSP)

#### 4.1 IncrementalView (incremental/view.rs:223)
```rust
pub struct IncrementalView {
    name: String,
    root_page: i64,  // ← Direct B-tree root page reference!
    index_root_page: i64,  // ← Another root page reference
    circuit: Option<Box<dyn DbspCircuit>>,
    // ...
}
```

#### 4.2 DbspStateCursors (incremental/persistence.rs)
```rust
pub struct DbspStateCursors {
    pub table_cursor: BTreeCursor,  // ← Active B-tree cursor
    pub index_cursor: BTreeCursor,  // ← Another active B-tree cursor
}
```

#### 4.3 MaterializedViewCursor (incremental/cursor.rs:47)
```rust
pub struct MaterializedViewCursor {
    btree_cursor: BTreeCursor,  // ← Active B-tree cursor
    view: Arc<Mutex<IncrementalView>>,
    tx_state: Arc<ViewTransactionState>,
    // ...
}
```

**VACUUM Impact:**
- Hidden tables `__turso_internal_dbsp_state_v*` are regular B-trees (handled normally)
- `IncrementalView.root_page` cache must be refreshed
- Schema cookie bump + re-parse will fix this automatically if properly implemented

### 5. Index Methods (FTS, Vector)

#### 5.1 FTS (Full-Text Search) - `core/index_method/fts.rs`

```rust
// HybridBTreeDirectory (lines 528-542)
pub struct HybridBTreeDirectory {
    pager: Arc<Pager>,
    btree_root_page: i64,  // ← B-tree root for FTS chunk storage
    // ...
}

// FtsIndexCursor (lines 1720-1722)
pub struct FtsIndexCursor {
    fts_dir_cursor: Option<BTreeCursor>,   // ← Active B-tree cursor
    btree_root_page: Option<i64>,          // ← Cached root page
    hybrid_directory: Option<HybridBTreeDirectory>,
    // ...
}
```

**VACUUM Impact:** FTS stores data in B-tree structures. These will be vacuumed as regular tables, but the FTS cursor's cached `btree_root_page` becomes stale.

#### 5.2 Vector Sparse Index - `core/index_method/toy_vector_sparse_ivf.rs`

```rust
pub struct VectorSparseInvertedIndexMethodCursor {
    inverted_index_cursor: Option<BTreeCursor>,  // ← Active B-tree cursor
    stats_cursor: Option<BTreeCursor>,            // ← Another B-tree cursor
    main_btree: Option<BTreeCursor>,              // ← Third B-tree cursor
    // ...
}
```

The vector index creates two additional B-tree indexes:
- `{index_name}_inverted_index`
- `{index_name}_stats`

**VACUUM Impact:** These are regular B-tree indexes in sqlite_schema. They'll be vacuumed normally, but open cursors must be closed first.

### 6. MVCC (Multi-Version Concurrency Control)

#### 6.1 RowID Structure (mvcc/database/mod.rs)
```rust
pub struct RowID {
    pub table_id: MVTableId,  // ← "Analogous to" root page - likely a logical ID
    pub row_id: RowKey,
}
```

#### 6.2 DualCursorPeek (mvcc/cursor.rs)
```rust
struct DualCursorPeek {
    mvcc_peek: CursorPeek,   // From MVCC in-memory store
    btree_peek: CursorPeek,  // From B-tree on disk
}
```

**VACUUM Impact:** MVCC requires coordination but is NOT fundamentally incompatible:
1. `table_id` is "analogous to" root page - likely a logical identifier that survives VACUUM
2. Dual-cursor merges B-tree with in-memory data - requires checkpoint before VACUUM
3. `.db-log` file may need invalidation after VACUUM
4. `btree_resident` flag semantics need consideration

**Solution:** Force MVCC checkpoint, clear in-memory store, then VACUUM normally.

### 7. Special Tables

#### 7.1 sqlite_sequence (autoincrement)
```rust
// core/schema.rs:150
pub const SQLITE_SEQUENCE_TABLE_NAME: &str = "sqlite_sequence";
```

Used to store AUTOINCREMENT counters. Must be preserved during VACUUM.

#### 7.2 sqlite_stat1 (analyze statistics)
```rust
// core/stats.rs:8-9
pub const STATS_TABLE: &str = "sqlite_stat1";
const STATS_QUERY: &str = "SELECT tbl, idx, stat FROM sqlite_stat1";
```

Stores ANALYZE statistics. After VACUUM, `analyze_stats` cache is refreshed automatically.

#### 7.3 DBSP Internal Tables
```rust
// core/schema.rs:151-152
pub const DBSP_TABLE_PREFIX: &str = "__turso_internal_dbsp_state_v";
pub const TURSO_INTERNAL_PREFIX: &str = "__turso_internal_";
```

Hidden tables for IVM. Treated as regular tables during VACUUM.

### 8. Page Cache (`core/storage/pager.rs`)

#### 8.1 PageCache Structure
```rust
page_cache: Arc<RwLock<PageCache>>,  // Caches page contents by page number
dirty_pages: Arc<RwLock<RoaringBitmap>>,  // Tracks dirty pages
```

**VACUUM Impact:** After VACUUM:
1. All cached pages are invalid (different content at same page numbers)
2. Must call `page_cache.clear()`
3. Dirty pages bitmap must be cleared

### 9. Connection State (`core/connection.rs`)

#### 9.1 Key Fields
```rust
pub struct Connection {
    pub schema: RwLock<Schema>,                        // Contains root pages
    pub(crate) view_transaction_states: AllViewsTxState,  // IVM deltas
    pub(super) attached_databases: RwLock<DatabaseCatalog>,  // Multiple pagers
    // ...
}
```

### Summary: B-tree Reference Inventory

| Component | Structure | Field | Impact |
|-----------|-----------|-------|--------|
| Schema | `BTreeTable` | `root_page` | Schema cookie invalidation fixes |
| Schema | `Index` | `root_page` | Schema cookie invalidation fixes |
| Schema | `IncrementalView` | `root_page`, `index_root_page` | Must re-read from schema |
| VDBE | `CursorType::BTreeTable` | Contains `Arc<BTreeTable>` | Programs must complete |
| VDBE | `CursorType::BTreeIndex` | Contains `Arc<Index>` | Programs must complete |
| FTS | `HybridBTreeDirectory` | `btree_root_page` | Cursor must be closed |
| FTS | `FtsIndexCursor` | `btree_root_page` | Cursor must be closed |
| Vector | `VectorSparseInvertedIndexMethodCursor` | 3 `BTreeCursor` fields | Cursors must be closed |
| IVM | `DbspStateCursors` | `table_cursor`, `index_cursor` | Cursors must be closed |
| IVM | `MaterializedViewCursor` | `btree_cursor` | Cursor must be closed |
| MVCC | `RowID` | `table_id` | Requires checkpoint + coordination |
| Pager | `PageCache` | Page contents | Must clear cache |

### Invalidation Strategy

After VACUUM completes:

1. **Bump schema cookie** (already done by SQLite-compatible approach)
   - Forces all connections to re-parse schema
   - `BTreeTable.root_page` and `Index.root_page` re-read from sqlite_schema

2. **Clear page cache**
   ```rust
   pager.page_cache.write().clear();
   ```

3. **For IVM**: Schema re-parse will:
   - Re-create `IncrementalView` objects
   - Read new root pages from sqlite_schema
   - Key: Don't cache root pages outside schema

4. **For Index Methods (FTS, Vector)**:
   - Cursors closed by precondition check ("no active statements")
   - Next cursor open will read new root page from `Index.root_page`
   - `Index.root_page` updated by schema re-parse

5. **For MVCC**:
   - Phase 1: Block VACUUM if MVCC enabled (safe, simple)
   - Phase 2: Proper coordination:
     1. Wait for active MVCC transactions to complete
     2. Force MVCC checkpoint (flush to B-tree)
     3. Clear/invalidate `.db-log` file
     4. Clear in-memory MVCC store
     5. Run VACUUM
     6. MVCC resumes with clean state
