/*
 * nexusview.h — C ABI for the NexusView engine (nexus-ffi).
 *
 * Ownership rules:
 *   - NexusDataset* / NexusView* are opaque handles. Never dereference them.
 *   - char* values returned by the engine are owned by the caller and MUST be
 *     released with nexus_string_free().
 *   - Handles are released with nexus_close() / nexus_view_free().
 *   - No function panics across this boundary. On failure they return
 *     NULL / 0 / -1 and nexus_last_error() holds a message (thread-local).
 */
#ifndef NEXUSVIEW_H
#define NEXUSVIEW_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct NexusDataset NexusDataset;
typedef struct NexusView NexusView;
typedef struct NexusGroupTree NexusGroupTree;

/* Group-tree positions are passed as int64 "items": >= 0 is a group node id;
 * < 0 is the data row (-item - 1). Child accessors return INT64_MIN when the
 * requested position is out of range. */
#define NEXUS_ITEM_INVALID INT64_MIN

/* ---- Diagnostics ------------------------------------------------------- */

/* Engine version, statically allocated (do not free). */
const char *nexus_version(void);

/* Last error on the calling thread. Valid until the next engine call on this
 * thread. Never NULL; empty string when there is no error. */
const char *nexus_last_error(void);

/* ---- Lifecycle --------------------------------------------------------- */

/* Open and index a file. `schema_json` may be NULL (auto-detect) or a JSON/YAML
 * parser-schema document. Returns NULL on failure. */
NexusDataset *nexus_open(const char *path, const char *schema_json);

/* Release a dataset handle (NULL-safe). */
void nexus_close(NexusDataset *ds);

/* ---- Metadata ---------------------------------------------------------- */

uint64_t nexus_row_count(const NexusDataset *ds);
uint32_t nexus_column_count(const NexusDataset *ds);

/* Column name as an owned C string (free with nexus_string_free), or NULL if
 * `col` is out of range. */
char *nexus_column_name(const NexusDataset *ds, uint32_t col);

/* ---- Search & views ---------------------------------------------------- */

/* Identity view over all rows. Free with nexus_view_free. NULL on error. */
NexusView *nexus_view_all(const NexusDataset *ds);

/* Run a search; returns a new filtered view. Free with nexus_view_free.
 * NULL on error (e.g. invalid query) — see nexus_last_error. */
NexusView *nexus_search(const NexusDataset *ds, const char *query);

/* Number of rows in a view. */
uint64_t nexus_view_count(const NexusView *view);

/* Underlying data-row index for a view position, or -1 if out of range. */
int64_t nexus_view_row_id(const NexusView *view, uint64_t row);

/* Cell value at (view row, col) as an owned C string (free with
 * nexus_string_free). Out-of-range yields ""; NULL only if a handle is NULL. */
char *nexus_view_cell(const NexusDataset *ds, const NexusView *view,
                      uint64_t row, uint32_t col);

/* Full raw record for a view position (original delimiters), owned C string. */
char *nexus_view_row_raw(const NexusDataset *ds, const NexusView *view,
                         uint64_t row);

/* Cell at an absolute data-row index (free with nexus_string_free). Used by the
 * grouping outline, whose leaves address absolute rows. "" if out of range. */
char *nexus_cell(const NexusDataset *ds, uint64_t row, uint32_t col);

/* All column values for a view row, joined by 0x01 (free with
 * nexus_string_free). One call per row instead of one per cell. */
char *nexus_view_row_cells(const NexusDataset *ds, const NexusView *view,
                           uint64_t row);

/* Stable multi-column sort (RF-05). cols[i] is a column index; ascending[i] is
 * 0 (descending) or non-zero (ascending), for `count` keys. Returns a new
 * sorted view (free with nexus_view_free); NULL on error. */
NexusView *nexus_sort(const NexusDataset *ds, const NexusView *view,
                      const uint32_t *cols, const uint8_t *ascending,
                      size_t count);

/* Release a view handle (NULL-safe). */
void nexus_view_free(NexusView *view);

/* ---- Grouping (RF-03) — drives an NSOutlineView ------------------------ */

/* Build a grouping tree for `view` over `count` columns (nesting order).
 * Free with nexus_group_free. NULL on error. */
NexusGroupTree *nexus_group(const NexusDataset *ds, const NexusView *view,
                            const uint32_t *cols, size_t count);

uint64_t nexus_group_root_count(const NexusGroupTree *tree);
int64_t  nexus_group_root_child(const NexusGroupTree *tree, uint64_t index);
uint64_t nexus_group_child_count(const NexusGroupTree *tree, int64_t item);
int64_t  nexus_group_child(const NexusGroupTree *tree, int64_t item, uint64_t index);
uint8_t  nexus_group_is_group(const NexusGroupTree *tree, int64_t item);
uint64_t nexus_group_count(const NexusGroupTree *tree, int64_t item);
int64_t  nexus_group_row(const NexusGroupTree *tree, int64_t item);

/* Group label as an owned C string (free with nexus_string_free); "" for rows. */
char *nexus_group_label(const NexusGroupTree *tree, int64_t item);

/* Release a grouping tree (NULL-safe). */
void nexus_group_free(NexusGroupTree *tree);

/* ---- Export (RF-10) ---------------------------------------------------- */

/* Export `view` to `path`. format: 0=CSV, 1=TSV, 2=JSON, 3=HTML.
 * `cols`/`ncols` selects columns in order; ncols==0 (or NULL cols) = all
 * columns. Hidden columns are excluded by omitting them.
 * Returns 0 on success, -1 on error (see nexus_last_error). */
int32_t nexus_export(const NexusDataset *ds, const NexusView *view,
                     uint32_t format, const uint32_t *cols, size_t ncols,
                     const char *path);

/* ---- Row tagging (Timeline Explorer "Tag") ----------------------------- */

/* Tag (tagged != 0) or untag an absolute data row. Tags persist across
 * filtering/sorting/grouping. */
void     nexus_set_tag(const NexusDataset *ds, uint64_t row, uint8_t tagged);
uint8_t  nexus_is_tagged(const NexusDataset *ds, uint64_t row);
uint64_t nexus_tagged_count(const NexusDataset *ds);
void     nexus_clear_tags(const NexusDataset *ds);

/* Views of tagged rows (free with nexus_view_free; NULL on error). */
NexusView *nexus_tagged_view(const NexusDataset *ds);
NexusView *nexus_intersect_tags(const NexusDataset *ds, const NexusView *view);

/* ---- Memory ------------------------------------------------------------ */

/* Free a C string previously returned by the engine (NULL-safe). */
void nexus_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* NEXUSVIEW_H */
