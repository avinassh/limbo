use crate::{CompletionError, Result};

const CHECKSUM_PAGE_SIZE: usize = 4096;
const CHECKSUM_SIZE: usize = 8;

#[derive(Clone)]
pub struct ChecksumContext {}

impl ChecksumContext {
    pub fn new() -> Self {
        ChecksumContext {}
    }

    pub fn add_checksum_to_page(&self, page: &mut [u8], page_id: usize) -> Result<()> {
        assert_eq!(
            page.len(),
            CHECKSUM_PAGE_SIZE,
            "page size must be 4096 bytes"
        );

        if page_id == 1 {
            // lets skip checksum verification for the first page (header page)
            let reserved_bytes = &page[CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE..];
            let reserved_bytes_zeroed = reserved_bytes.iter().all(|&b| b == 0);
            assert!(
                reserved_bytes_zeroed,
                "last reserved bytes must be empty/zero, but found non-zero bytes on page {page_id}"
            );
            return Ok(());
        }

        // compute checksum on the actual page data (excluding the reserved checksum area)
        let actual_page = &page[..CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE];
        let checksum = self.compute_checksum(actual_page);

        // write checksum directly to the reserved area at the end of the page
        let checksum_bytes = checksum.to_le_bytes();
        assert_eq!(checksum_bytes.len(), CHECKSUM_SIZE);
        page[CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE..].copy_from_slice(&checksum_bytes);
        Ok(())
    }

    pub fn verify_and_strip_checksum(
        &self,
        page: &mut [u8],
        page_id: usize,
    ) -> std::result::Result<(), CompletionError> {
        assert_eq!(
            page.len(),
            CHECKSUM_PAGE_SIZE,
            "page size must be 4096 bytes"
        );

        if page_id == 1 {
            // lets skip checksum verification for the first page (header page)
            return Ok(());
        }

        // extract data and checksum portions
        let actual_page = &page[..CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE];
        let stored_checksum_bytes = &page[CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE..];
        let stored_checksum = u64::from_le_bytes(stored_checksum_bytes.try_into().unwrap());

        // verify checksum
        let computed_checksum = self.compute_checksum(actual_page);
        if stored_checksum != computed_checksum {
            return Err(CompletionError::ChecksumMismatch {
                page_id,
                expected: stored_checksum,
                actual: computed_checksum,
            });
        }
        tracing::trace!("checksum verified (page_id={page_id})");
        // zero out the checksum area in-place
        // page[CHECKSUM_PAGE_SIZE - CHECKSUM_SIZE..].fill(0);
        Ok(())
    }

    fn compute_checksum(&self, data: &[u8]) -> u64 {
        twox_hash::XxHash3_64::oneshot(data)
    }
}

impl Default for ChecksumContext {
    fn default() -> Self {
        Self::new()
    }
}
