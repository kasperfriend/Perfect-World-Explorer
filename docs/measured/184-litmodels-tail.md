# 1.8.4 `litmodels` Trailer Tail (Last 288 Bytes)

- **Status**: **Blocked** (Source: Drive)
- **Target files**:
  - `pw184_e585_t223/element/litmodels.pkx1` (Drive ID `1jqF0TBA5TjfxoEn9uF8BJ5Q87xpWQfvj`, size **3,681,422,489 bytes**)
  - `pw184_e585_t223/SPLIT ARCHIVES/litmodels.pkx1.zip.037` (Drive ID `1z8ciQ4IdwmV3-fz8gmhdWqIQygzNyEYE`, size **23,998,995 bytes**)
- **What was tried**:
  1. Full Drive download of `litmodels.pkx1` (3,681,422,489 bytes).
  2. Full Drive download of `litmodels.pkx1.zip.037` (23,998,995 bytes).
  3. Drive content read of `litmodels.pkx1.zip.037` (23,998,995 bytes).
- **Exact errors**:
  - Drive download: `File too large for download, over limit of 10 MB. For downloading larger files, use the standard Google Drive API. See https://developers.google.com/workspace/drive/api/guides/manage-downloads#curl`
  - Drive content read: `File content cannot be retrieved for item 1z8ciQ4IdwmV3-fz8gmhdWqIQygzNyEYE due to unsupported mime type.`
- **Consequence**: The last 288 bytes of `litmodels` could not be read through Drive. Section 3.3 (1.8.4 tail values in the compatibility matrix) remains blocked until a ≤ 10 MB tail slice or direct Google Drive API read is available.
