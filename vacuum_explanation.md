# SQLite VACUUM: Complete Walkthrough

This document explains SQLite's VACUUM implementation in detail, including how B-trees are rewritten and why certain design decisions were made.

## Table of Contents

1. [Overview](#overview)
2. [The VACUUM Algorithm](#the-vacuum-algorithm)
3. [Phase-by-Phase Walkthrough](#phase-by-phase-walkthrough)
4. [B-tree Rewriting Explained](#b-tree-rewriting-explained)
5. [The Copy-Back Mechanism](#the-copy-back-mechanism)
6. [Why Readers Are Blocked](#why-readers-are-blocked)
7. [VACUUM INTO Difference](#vacuum-into-difference)
8. [Key Code Snippets](#key-code-snippets)

---

## Overview

VACUUM rebuilds the database file from scratch to:
- Reclaim free space from deleted rows
- Defragment B-trees for better locality
- Apply pending page size changes
- Apply pending auto-vacuum mode changes

The key insight is that **VACUUM doesn't modify the database in place**. Instead, it:
1. Creates a fresh database
2. Copies all content via SQL
3. Replaces the original with the fresh copy

This is safer and simpler than trying to reorganize pages in place.

---

## The VACUUM Algorithm

```
┌─────────────────────────────────────────────────────────────────┐
│                     VACUUM Algorithm                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. Precondition checks                                         │
│     - Must be in autocommit mode                                │
│     - No other statements active                                │
│                                                                 │
│  2. Create temp database                                        │
│     - ATTACH '' AS vacuum_xxxxx                                 │
│     - Configure page size, auto_vacuum, etc.                    │
│                                                                 │
│  3. Acquire EXCLUSIVE lock on main database                     │
│     - Blocks all readers and writers                            │
│                                                                 │
│  4. Copy schema (empty tables/indexes)                          │
│     - Execute CREATE TABLE statements in temp DB                │
│     - Execute CREATE INDEX statements in temp DB                │
│                                                                 │
│  5. Copy data                                                   │
│     - INSERT INTO temp.t SELECT * FROM main.t                   │
│     - For each table                                            │
│                                                                 │
│  6. Copy views, triggers, virtual tables                        │
│     - These have no storage, just schema entries                │
│                                                                 │
│  7. Copy metadata                                               │
│     - Schema version (incremented by 1)                         │
│     - User version, application ID, etc.                        │
│                                                                 │
│  8. Copy temp database back to main                             │
│     - Page-by-page copy using backup API                        │
│     - This is where B-trees get new page numbers                │
│                                                                 │
│  9. Cleanup                                                     │
│     - Detach temp database                                      │
│     - Delete temp file                                          │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Phase-by-Phase Walkthrough

### Phase 1: Precondition Checks

```c
// vacuum.c:169-188
int sqlite3RunVacuum(
  char **pzErrMsg,        // Write error message here
  sqlite3 *db,            // Database connection
  int iDb,                // Which database to vacuum
  sqlite3_value *pOut     // NULL for regular VACUUM, filename for INTO
){
  // Must be in autocommit mode
  if( !db->autoCommit ){
    sqlite3SetString(pzErrMsg, db,
        "cannot VACUUM from within a transaction");
    return SQLITE_ERROR;
  }

  // No other statements can be active
  if( db->nVdbeActive>1 ){
    sqlite3SetString(pzErrMsg, db,
        "cannot VACUUM - SQL statements in progress");
    return SQLITE_ERROR;
  }
```

**Why these restrictions?**

- **Autocommit mode**: VACUUM needs to modify the entire database atomically. If we're inside a transaction that later rolls back, we'd have a half-vacuumed database.

- **No active statements**: Other statements have open cursors with cached page references. VACUUM changes page assignments, so those cursors would point to wrong data.

### Phase 2: Create Temp Database

```c
// vacuum.c:209-232
// Generate random name to avoid conflicts
sqlite3_randomness(sizeof(iRandom), &iRandom);
sqlite3_snprintf(sizeof(zDbVacuum), zDbVacuum, "vacuum_%016llx", iRandom);

// For VACUUM INTO, use the target filename
// For regular VACUUM, use empty string (creates temp file)
if( pOut ){
  zOut = sqlite3_value_text(pOut);
} else {
  zOut = "";
}

// Attach the temporary database
rc = execSqlF(db, pzErrMsg, "ATTACH %Q AS %s", zOut, zDbVacuum);
```

The temp database is attached with a random name like `vacuum_a1b2c3d4e5f6g7h8` to avoid conflicts if multiple connections try to VACUUM simultaneously.

### Phase 3: Acquire Exclusive Lock

```c
// vacuum.c:264-279
// Begin transaction on temp database
rc = execSql(db, pzErrMsg, "BEGIN");
if( rc!=SQLITE_OK ) goto end_of_vacuum;

// Acquire EXCLUSIVE lock on main database
// wrflag=2 means EXCLUSIVE (not just write)
// wrflag=1 would be normal write
// wrflag=0 would be read-only
rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);
//                                  ^^^^^^^^^^^^^^
//                          Regular VACUUM: 2 (EXCLUSIVE)
//                          VACUUM INTO:    0 (READ)
```

**The EXCLUSIVE lock (wrflag=2) is critical:**

```c
// btree.c - Lock levels
// SHARED_LOCK    = 1  (read)
// RESERVED_LOCK  = 2  (intent to write)
// PENDING_LOCK   = 3  (waiting for readers to finish)
// EXCLUSIVE_LOCK = 4  (no other access allowed)
```

When `sqlite3BtreeBeginTrans` is called with `wrflag=2`:
1. Acquires PENDING_LOCK (blocks new readers)
2. Waits for existing readers to finish
3. Upgrades to EXCLUSIVE_LOCK
4. Now NO other connection can read or write

### Phase 4: Copy Schema

```c
// vacuum.c:294-326
// Force new objects to be created in vacuum_db
db->init.iDb = nDb;  // nDb = index of vacuum database

// Copy table definitions (but NOT data yet)
rc = execSqlF(db, pzErrMsg,
    "SELECT sql FROM \"%w\".sqlite_schema"
    " WHERE type='table'"
    " AND name<>'sqlite_sequence'"
    " AND coalesce(rootpage,1)>0",  // Skip virtual tables (rootpage=0)
    zDbMain
);

// Copy index definitions
rc = execSqlF(db, pzErrMsg,
    "SELECT sql FROM \"%w\".sqlite_schema"
    " WHERE type='index'",
    zDbMain
);
```

**What happens here:**

1. Read each `CREATE TABLE` statement from `sqlite_schema`
2. Execute it in the temp database → creates empty table with new root page
3. Read each `CREATE INDEX` statement
4. Execute it in temp database → creates empty index with new root page

**Security check:**

```c
// vacuum.c:43-52
// Only allow CREATE and INSERT statements from schema
// This prevents SQL injection via corrupted sqlite_schema
if( zSubSql
 && (strncmp(zSubSql,"CRE",3)==0 || strncmp(zSubSql,"INS",3)==0)
){
  rc = execSql(db, pzErrMsg, zSubSql);
}
```

### Phase 5: Copy Data

```c
// vacuum.c:317-326
// Generate INSERT statements for each table
rc = execSqlF(db, pzErrMsg,
    "SELECT 'INSERT INTO %s.'||quote(name)"
    "||' SELECT*FROM\"%w\".'||quote(name)"
    " FROM %s.sqlite_schema"
    " WHERE type='table'"
    " AND coalesce(rootpage,1)>0",
    zDbVacuum,    // destination: vacuum_xxxxx
    zDbMain,      // source: main
    zDbVacuum     // read table list from vacuum_xxxxx.sqlite_schema
);
```

This generates and executes statements like:
```sql
INSERT INTO vacuum_xxxxx."users" SELECT * FROM "main"."users";
INSERT INTO vacuum_xxxxx."orders" SELECT * FROM "main"."orders";
```

**This is where the magic happens:**

- Rows are read from main database B-trees
- Inserted into temp database B-trees
- Temp B-trees are built fresh, compactly, in order
- No fragmentation, no wasted space

### Phase 6: Copy Views and Triggers

```c
// vacuum.c:333-339
rc = execSqlF(db, pzErrMsg,
    "INSERT INTO %s.sqlite_schema"
    " SELECT*FROM \"%w\".sqlite_schema"
    " WHERE type IN('view','trigger')"
    " OR (type='table' AND rootpage=0)",  // Virtual tables
    zDbVacuum, zDbMain
);
```

Views, triggers, and virtual tables have no storage - they're just schema entries. We copy them directly to `sqlite_schema`.

### Phase 7: Copy Metadata

```c
// vacuum.c:348-376
static const unsigned char aCopy[] = {
  BTREE_SCHEMA_VERSION,     1,  // Add 1 to schema version!
  BTREE_DEFAULT_CACHE_SIZE, 0,  // Preserve cache size
  BTREE_TEXT_ENCODING,      0,  // Preserve encoding (UTF-8, etc)
  BTREE_USER_VERSION,       0,  // Preserve PRAGMA user_version
  BTREE_APPLICATION_ID,     0,  // Preserve PRAGMA application_id
};

for(i=0; i<ArraySize(aCopy); i+=2){
  // Read from main database
  sqlite3BtreeGetMeta(pMain, aCopy[i], &meta);
  // Write to temp database (with optional increment)
  rc = sqlite3BtreeUpdateMeta(pTemp, aCopy[i], meta + aCopy[i+1]);
}
```

**The schema version increment is crucial:**
- Other connections cache the schema version
- When they see it changed, they re-parse the schema
- This ensures they pick up new root page numbers

### Phase 8: Copy Back to Main (The Critical Phase)

```c
// vacuum.c:378-395
if( pOut==0 ){
  // Regular VACUUM: copy temp back to main
  rc = sqlite3BtreeCopyFile(pMain, pTemp);
}
// For VACUUM INTO: skip this - temp IS the result
```

Let's look at `sqlite3BtreeCopyFile`:

```c
// backup.c:718-766
int sqlite3BtreeCopyFile(Btree *pTo, Btree *pFrom){
  sqlite3_backup b;

  // Initialize backup structure
  memset(&b, 0, sizeof(b));
  b.pSrcDb = pFrom->db;
  b.pSrc = pFrom;
  b.pDest = pTo;
  b.iNext = 1;  // Start from page 1

  // Copy ALL pages
  sqlite3_backup_step(&b, 0x7FFFFFFF);  // 0x7FFFFFFF = copy everything

  return sqlite3_backup_finish(&b);
}
```

**What `sqlite3_backup_step` does:**

```c
// backup.c:440-560 (simplified)
int sqlite3_backup_step(sqlite3_backup *p, int nPage){
  // For each page to copy...
  for(/* each page */){
    // Read page from source
    DbPage *pSrcPg;
    sqlite3PagerGet(p->pSrc->pBt->pPager, iSrcPg, &pSrcPg);

    // Get destination page (may allocate new)
    DbPage *pDestPg;
    sqlite3PagerGet(p->pDest->pBt->pPager, iDestPg, &pDestPg);

    // Copy the raw bytes
    memcpy(sqlite3PagerGetData(pDestPg),
           sqlite3PagerGetData(pSrcPg),
           p->pSrc->pBt->pageSize);

    // Mark destination page dirty
    sqlite3PagerDirty(pDestPg);
  }
}
```

---

## B-tree Rewriting Explained

This is the core of why VACUUM works. Let's trace through an example.

### Before VACUUM

```
Main Database File:
┌─────────────────────────────────────────────────────────────┐
│ Page 1: Database Header + sqlite_schema root                │
├─────────────────────────────────────────────────────────────┤
│ Page 2: sqlite_schema leaf (table definitions)              │
├─────────────────────────────────────────────────────────────┤
│ Page 3: "users" table root                                  │
├─────────────────────────────────────────────────────────────┤
│ Page 4: "users" internal node                               │
├─────────────────────────────────────────────────────────────┤
│ Page 5: "users" leaf (rows 1-50)                            │
├─────────────────────────────────────────────────────────────┤
│ Page 6: FREE (was "users" leaf, rows deleted)               │
├─────────────────────────────────────────────────────────────┤
│ Page 7: FREE (was "users" leaf, rows deleted)               │
├─────────────────────────────────────────────────────────────┤
│ Page 8: "users" leaf (rows 100-150)                         │
├─────────────────────────────────────────────────────────────┤
│ Page 9: "orders" table root                                 │
├─────────────────────────────────────────────────────────────┤
│ Page 10: "orders" leaf                                      │
├─────────────────────────────────────────────────────────────┤
│ Page 11: FREE                                               │
├─────────────────────────────────────────────────────────────┤
│ Page 12: "users_email_idx" index root                       │
├─────────────────────────────────────────────────────────────┤
│ Page 13: "users_email_idx" leaf                             │
└─────────────────────────────────────────────────────────────┘

sqlite_schema contents:
  type='table', name='users',    rootpage=3,  sql='CREATE TABLE users...'
  type='table', name='orders',   rootpage=9,  sql='CREATE TABLE orders...'
  type='index', name='users_idx', rootpage=12, sql='CREATE INDEX...'

Problems:
  - 3 free pages (6, 7, 11) wasting space
  - "users" data fragmented (pages 5 and 8 not adjacent)
  - File is 13 pages when it could be 10
```

### During VACUUM: Building Temp Database

```
Step 1: CREATE TABLE users(...) in temp DB
        → Allocates page 2 for users root (page 1 is header)

Step 2: CREATE TABLE orders(...) in temp DB
        → Allocates page 3 for orders root

Step 3: CREATE INDEX users_email_idx in temp DB
        → Allocates page 4 for index root

Step 4: INSERT INTO temp.users SELECT * FROM main.users
        → Inserts rows, allocates pages 5, 6 for leaves
        → B-tree built compactly, in order

Step 5: INSERT INTO temp.orders SELECT * FROM main.orders
        → Allocates page 7 for leaf

Step 6: Index automatically populated during inserts
        → Allocates pages 8, 9 for index leaves

Temp Database (compact, no holes):
┌─────────────────────────────────────────────────────────────┐
│ Page 1: Database Header + sqlite_schema root                │
├─────────────────────────────────────────────────────────────┤
│ Page 2: "users" table root         ← Was page 3             │
├─────────────────────────────────────────────────────────────┤
│ Page 3: "orders" table root        ← Was page 9             │
├─────────────────────────────────────────────────────────────┤
│ Page 4: "users_email_idx" root     ← Was page 12            │
├─────────────────────────────────────────────────────────────┤
│ Page 5: "users" leaf (all rows)    ← Data consolidated      │
├─────────────────────────────────────────────────────────────┤
│ Page 6: "users" leaf (continued)                            │
├─────────────────────────────────────────────────────────────┤
│ Page 7: "orders" leaf                                       │
├─────────────────────────────────────────────────────────────┤
│ Page 8: "users_email_idx" leaf                              │
├─────────────────────────────────────────────────────────────┤
│ Page 9: "users_email_idx" leaf                              │
└─────────────────────────────────────────────────────────────┘

sqlite_schema in temp DB:
  type='table', name='users',    rootpage=2,  sql='CREATE TABLE users...'
  type='table', name='orders',   rootpage=3,  sql='CREATE TABLE orders...'
  type='index', name='users_idx', rootpage=4, sql='CREATE INDEX...'
```

### After Copy-Back

The temp database pages are copied over the main database file:

```
Main Database File (after VACUUM):
┌─────────────────────────────────────────────────────────────┐
│ Page 1: Database Header + sqlite_schema                     │
├─────────────────────────────────────────────────────────────┤
│ Page 2: "users" table root         ← NEW location           │
├─────────────────────────────────────────────────────────────┤
│ Page 3: "orders" table root        ← NEW location           │
├─────────────────────────────────────────────────────────────┤
│ Page 4: "users_email_idx" root     ← NEW location           │
├─────────────────────────────────────────────────────────────┤
│ Page 5: "users" leaf                                        │
├─────────────────────────────────────────────────────────────┤
│ Page 6: "users" leaf                                        │
├─────────────────────────────────────────────────────────────┤
│ Page 7: "orders" leaf                                       │
├─────────────────────────────────────────────────────────────┤
│ Page 8: "users_email_idx" leaf                              │
├─────────────────────────────────────────────────────────────┤
│ Page 9: "users_email_idx" leaf                              │
└─────────────────────────────────────────────────────────────┘
[File truncated here - was 13 pages, now 9 pages]

Results:
  - File shrunk from 13 pages to 9 pages
  - No free pages (freelist empty)
  - "users" data now contiguous (pages 5-6)
  - All B-trees rebuilt compactly
```

---

## The Copy-Back Mechanism

The copy-back uses SQLite's backup API internally via normal pager APIs (NOT raw WAL insertion):

```c
// backup.c:255-269 - backupOnePage()
static int backupOnePage(sqlite3_backup *p, Pgno iSrcPg, const u8 *zSrcData, int bUpdate){
  // For each destination page:
  sqlite3PagerGet(pDestPager, iDest, &pDestPg, 0);  // Get dest page into cache
  sqlite3PagerWrite(pDestPg);                        // Mark dirty (engages journal/WAL)
  memcpy(zOut, zIn, nCopy);                         // Copy raw page content
  sqlite3PagerUnref(pDestPg);
}
```

**Key insight:** SQLite does NOT have raw WAL insertion APIs. All writes go through the pager layer:
1. `sqlite3PagerGet()` - fetches page into cache
2. `sqlite3PagerWrite()` - marks page dirty
3. `memcpy()` - copies the raw bytes
4. At commit time, pager writes dirty pages to appropriate destination

**WAL vs Rollback Mode Difference** (from `pager.c:6500-6517`):

```c
if( pagerUseWal(pPager) ){
  // WAL MODE: dirty pages go to WAL file
  pList = sqlite3PcacheDirtyList(pPager->pPCache);
  rc = pagerWalFrames(pPager, pList, pPager->dbSize, 1);
}else{
  // ROLLBACK MODE: dirty pages go to main DB with journal protection
  // (original pages saved to rollback journal first)
}
```

**The rollback journal code path** (in `pager_write()`) has this assertion:
```c
assert( pagerUseWal(pPager)==0 );  // Only runs when NOT in WAL mode
```

This confirms: in WAL mode, no rollback journal is used. Dirty pages go to WAL, then checkpoint moves them to main DB.

**Why not atomic rename?** From vacuum.c comments (lines 97-103):
> "But that will not work if other processes are attached to the original database."

Each connection has its own file handle. After rename, those handles point to a deleted inode → broken connections.

---

## Why Readers Are Blocked

Consider what happens if a reader is active during copy-back:

```
Time T1: Reader opens cursor on "users" table
         Reader caches: rootpage=3 (from old sqlite_schema)
         Reader reads page 3, sees B-tree root
         Reader follows pointer to page 5 (leaf)

Time T2: VACUUM copy-back begins
         Page 2 overwritten (now contains "users" root)
         Page 3 overwritten (now contains "orders" root!)

Time T3: Reader continues
         Reader tries to read more from "users"
         Reader still thinks page 3 is "users" root
         Reader reads page 3 → gets "orders" data!
         CORRUPTION / WRONG RESULTS
```

Even worse scenario with B-tree traversal:

```
Time T1: Reader traversing "users" B-tree
         Currently at internal node, page 4
         Next step: follow pointer to child page 8

Time T2: VACUUM copy-back
         Page 8 overwritten with "users_email_idx" leaf
         (It was "users" leaf before, index leaf after)

Time T3: Reader follows pointer to page 8
         Expects: "users" table leaf with row data
         Gets: "users_email_idx" index entries
         Tries to decode index entry as row → GARBAGE
```

**This is why EXCLUSIVE lock is required.**

---

## VACUUM INTO Difference

`VACUUM INTO 'newfile.db'` is different:

```c
// vacuum.c:264-270
rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);
//                                  ▲▲▲▲▲▲▲▲▲▲▲▲▲▲
//                          pOut==0 (regular): wrflag=2 (EXCLUSIVE)
//                          pOut!=0 (INTO):    wrflag=0 (READ-ONLY)
```

With VACUUM INTO:
- Source database is only **read**, never modified
- Destination is a **new file** that doesn't exist yet
- No copy-back phase
- Readers can continue on source database

```
VACUUM INTO 'backup.db':

Source (main.db):              Destination (backup.db):
┌──────────────────┐           ┌──────────────────┐
│ Page 1: Header   │ ──READ──► │ Page 1: Header   │
│ Page 2: users    │ ──READ──► │ Page 2: users    │
│ Page 3: FREE     │           │ (no free pages)  │
│ Page 4: orders   │ ──READ──► │ Page 3: orders   │
│ Page 5: FREE     │           │                  │
└──────────────────┘           └──────────────────┘
        │                              │
        │                              ▼
   Unchanged!                 New compact file
   Readers OK!
```

---

## Key Code Snippets

### Entry Point: sqlite3Vacuum (vacuum.c:105-138)

```c
void sqlite3Vacuum(Parse *pParse, Token *pNm, Expr *pInto){
  Vdbe *v = sqlite3GetVdbe(pParse);
  int iDb = 0;

  // Parse database name if provided (VACUUM schema_name)
  if( pNm ){
    iDb = sqlite3TwoPartName(pParse, pNm, pNm, &pNm);
    if( iDb<0 ) goto build_vacuum_end;
  }

  // Cannot VACUUM temp database (iDb==1)
  if( iDb!=1 ){
    int iIntoReg = 0;

    // Handle VACUUM INTO filename
    if( pInto && sqlite3ResolveSelfReference(pParse,0,0,pInto,0)==0 ){
      iIntoReg = ++pParse->nMem;
      sqlite3ExprCode(pParse, pInto, iIntoReg);
    }

    // Emit single OP_Vacuum instruction
    sqlite3VdbeAddOp2(v, OP_Vacuum, iDb, iIntoReg);
    sqlite3VdbeUsesBtree(v, iDb);
  }

build_vacuum_end:
  sqlite3ExprDelete(pParse->db, pInto);
}
```

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

### Full sqlite3RunVacuum (vacuum.c:143-427)

```c
int sqlite3RunVacuum(
  char **pzErrMsg,
  sqlite3 *db,
  int iDb,
  sqlite3_value *pOut
){
  int rc = SQLITE_OK;
  Btree *pMain;           // Main database B-tree
  Btree *pTemp;           // Temp database B-tree
  int saved_flags;        // Saved db flags
  i64 saved_nChange;      // Saved change counter
  i64 saved_totalChange;  // Saved total changes
  u32 saved_openFlags;    // Saved open flags
  u8 saved_mTrace;        // Saved trace flags
  Db *pDb = 0;            // Database being vacuumed

  // ... precondition checks ...

  // Save state that might be modified
  saved_flags = db->flags;
  saved_nChange = db->nChange;
  saved_totalChange = db->nTotalChange;
  saved_mTrace = db->mTrace;
  db->flags |= SQLITE_WriteSchema | SQLITE_IgnoreChecks;
  db->mTrace = 0;

  // Create temp database name
  sqlite3_randomness(sizeof(iRandom), &iRandom);
  sqlite3_snprintf(sizeof(zDbVacuum), zDbVacuum,
                   "vacuum_%016llx", iRandom);

  // Attach temp database
  rc = execSqlF(db, pzErrMsg, "ATTACH %Q AS %s", zOut, zDbVacuum);
  if( rc!=SQLITE_OK ) goto end_of_vacuum;

  pDb = &db->aDb[db->nDb-1];
  pTemp = pDb->pBt;

  // Configure temp database to match main
  nRes = sqlite3BtreeGetRequestedReserve(pMain);
  sqlite3BtreeSetPageSize(pTemp, sqlite3BtreeGetPageSize(pMain), nRes, 0);
  sqlite3BtreeSetAutoVacuum(pTemp,
      db->nextAutovac>=0 ? db->nextAutovac :
                           sqlite3BtreeGetAutoVacuum(pMain));

  // Begin transactions
  rc = execSql(db, pzErrMsg, "BEGIN");
  rc = sqlite3BtreeBeginTrans(pMain, pOut==0 ? 2 : 0, 0);

  // Copy schema
  db->init.iDb = nDb;
  rc = execSqlF(db, pzErrMsg,
      "SELECT sql FROM \"%w\".sqlite_schema"
      " WHERE type='table' AND name<>'sqlite_sequence'"
      " AND coalesce(rootpage,1)>0",
      zDbMain);

  // Copy data
  rc = execSqlF(db, pzErrMsg,
      "SELECT 'INSERT INTO %s.'||quote(name)"
      "||' SELECT*FROM\"%w\".'||quote(name)"
      " FROM %s.sqlite_schema"
      " WHERE type='table' AND coalesce(rootpage,1)>0",
      zDbVacuum, zDbMain, zDbVacuum);

  // Copy views and triggers
  rc = execSqlF(db, pzErrMsg,
      "INSERT INTO %s.sqlite_schema"
      " SELECT*FROM \"%w\".sqlite_schema"
      " WHERE type IN('view','trigger')"
      " OR(type='table' AND rootpage=0)",
      zDbVacuum, zDbMain);

  // Copy metadata (schema version incremented by 1)
  for(i=0; i<ArraySize(aCopy); i+=2){
    sqlite3BtreeGetMeta(pMain, aCopy[i], &meta);
    rc = sqlite3BtreeUpdateMeta(pTemp, aCopy[i], meta+aCopy[i+1]);
  }

  // Copy temp DB back to main (for regular VACUUM)
  if( pOut==0 ){
    rc = sqlite3BtreeCopyFile(pMain, pTemp);
  }

  // Commit and cleanup
  rc = execSql(db, pzErrMsg, "COMMIT");

end_of_vacuum:
  // Restore saved state
  db->flags = saved_flags;
  db->nChange = saved_nChange;
  db->nTotalChange = saved_totalChange;
  db->mTrace = saved_mTrace;

  // Detach temp database
  sqlite3BtreeClose(pTemp);

  return rc;
}
```

### sqlite3BtreeCopyFile (backup.c:718-766)

```c
int sqlite3BtreeCopyFile(Btree *pTo, Btree *pFrom){
  int rc;
  sqlite3_file *pFd;
  sqlite3_backup b;
  sqlite3BtreeEnter(pTo);
  sqlite3BtreeEnter(pFrom);

  // Initialize backup object for page-by-page copy
  memset(&b, 0, sizeof(b));
  b.pSrcDb = pFrom->db;
  b.pSrc = pFrom;
  b.pDest = pTo;
  b.iNext = 1;

  // Copy all pages at once
  // 0x7FFFFFFF means "copy everything"
  sqlite3_backup_step(&b, 0x7FFFFFFF);
  assert( b.rc!=SQLITE_OK );

  rc = sqlite3_backup_finish(&b);

  // Allow page size changes after copy
  if( rc==SQLITE_OK ){
    pTo->pBt->btsFlags &= ~BTS_PAGESIZE_FIXED;
  }

  sqlite3BtreeLeave(pFrom);
  sqlite3BtreeLeave(pTo);
  return rc;
}
```

---

## Summary

1. **VACUUM rebuilds the database from scratch** - Creates temp DB, copies via SQL, replaces original

2. **B-trees get new page numbers** - Fresh allocation means compact, defragmented trees

3. **Schema cookie increment** - Forces other connections to re-parse schema with new root pages

4. **EXCLUSIVE lock required** - Copy-back phase overwrites pages, readers would see garbage

5. **VACUUM INTO is less restrictive** - Only reads source, writes to new file, readers OK

6. **The backup API does the heavy lifting** - Page-by-page copy handles the actual file replacement

---

## WAL Mode Specifics (Important for Turso)

### No Rollback Journal in WAL Mode

SQLite does NOT switch to rollback journal mode for VACUUM. The comment in vacuum.c about "2x disk space for rollback journal" applies only to rollback mode.

In WAL mode:
- `sqlite3PagerWrite()` marks pages dirty
- At commit, `pagerWalFrames()` writes dirty pages to WAL
- Checkpoint copies WAL pages to main DB file
- No rollback journal needed

### No Raw WAL Insertion APIs

SQLite uses normal pager APIs for all page writes:
```c
sqlite3PagerGet()   // Get page into cache
sqlite3PagerWrite() // Mark dirty
memcpy()            // Copy content
// At commit: pager routes to WAL or journal automatically
```

### Why Atomic Rename Doesn't Work

From vacuum.c:
> "But that will not work if other processes are attached to the original database."

- Each connection has its own file handle
- Rename deletes original file
- Other connections' handles point to deleted inode
- Result: crashes and data corruption

### Turso Implementation Approach (WAL-only)

```
1. Acquire exclusive lock via checkpoint TRUNCATE mode
2. VACUUM INTO temp file (creates defragmented copy)
3. For each page in temp:
   - pager.get_page(main_db, page_num)
   - pager.write_page() → marks dirty
   - copy temp page content to main page
4. Commit → dirty pages go to WAL
5. Checkpoint → WAL pages go to main DB file
6. Delete temp file
7. Schema cookie bump invalidates other connections' caches
```

This approach:
- Preserves file handles (no rename)
- Works with multiple connections
- Uses existing async pager infrastructure
- Crash-safe via WAL
