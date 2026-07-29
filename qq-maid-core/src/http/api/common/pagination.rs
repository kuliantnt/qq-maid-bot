//! 管理 API 共用分页请求与响应。

use serde::{Deserialize, Serialize};

use super::ApiError;

pub(crate) const DEFAULT_PAGE: u64 = 1;
pub(crate) const DEFAULT_PAGE_SIZE: u64 = 20;
pub(crate) const MAX_PAGE_SIZE: u64 = 100;

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct PaginationRequest {
    pub(crate) page: Option<u64>,
    pub(crate) page_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedPagination {
    page: u64,
    page_size: u64,
    offset: u64,
}

impl PaginationRequest {
    pub(crate) fn validate(self) -> Result<ValidatedPagination, ApiError> {
        let page = self.page.unwrap_or(DEFAULT_PAGE);
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page == 0 {
            return Err(ApiError::validation(
                "page must be greater than or equal to 1",
            ));
        }
        if page_size == 0 {
            return Err(ApiError::validation(
                "page_size must be greater than or equal to 1",
            ));
        }
        if page_size > MAX_PAGE_SIZE {
            return Err(ApiError::validation(format!(
                "page_size must not exceed {MAX_PAGE_SIZE}"
            )));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|value| value.checked_mul(page_size))
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| ApiError::validation("pagination offset is too large"))?;
        Ok(ValidatedPagination {
            page,
            page_size,
            offset,
        })
    }
}

impl ValidatedPagination {
    pub(crate) fn page(self) -> u64 {
        self.page
    }

    pub(crate) fn page_size(self) -> u64 {
        self.page_size
    }

    pub(crate) fn offset(self) -> u64 {
        self.offset
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PagedResponse<T> {
    pub(crate) items: Vec<T>,
    pub(crate) page: u64,
    pub(crate) page_size: u64,
    pub(crate) total: u64,
    pub(crate) total_pages: u64,
}

impl<T> PagedResponse<T> {
    pub(crate) fn new(items: Vec<T>, pagination: ValidatedPagination, total: u64) -> Self {
        let page_size = pagination.page_size;
        let total_pages = if total == 0 {
            0
        } else {
            total / page_size + u64::from(!total.is_multiple_of(page_size))
        };
        Self {
            items,
            page: pagination.page(),
            page_size,
            total,
            total_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_uses_defaults_and_accepts_custom_values() {
        let default = PaginationRequest::default().validate().unwrap();
        assert_eq!(default.page(), 1);
        assert_eq!(default.page_size(), 20);
        assert_eq!(default.offset(), 0);

        let custom = PaginationRequest {
            page: Some(3),
            page_size: Some(7),
        }
        .validate()
        .unwrap();
        assert_eq!(custom.page(), 3);
        assert_eq!(custom.page_size(), 7);
        assert_eq!(custom.offset(), 14);
    }

    #[test]
    fn pagination_rejects_zero_and_oversized_values() {
        assert!(
            PaginationRequest {
                page: Some(0),
                page_size: None,
            }
            .validate()
            .is_err()
        );
        assert!(
            PaginationRequest {
                page: None,
                page_size: Some(0),
            }
            .validate()
            .is_err()
        );
        assert!(
            PaginationRequest {
                page: None,
                page_size: Some(MAX_PAGE_SIZE + 1),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn paged_response_calculates_zero_and_partial_pages_without_overflow() {
        let pagination = PaginationRequest::default().validate().unwrap();
        let empty = PagedResponse::<()>::new(Vec::new(), pagination, 0);
        assert_eq!(empty.total_pages, 0);

        let partial = PagedResponse::<()>::new(Vec::new(), pagination, 21);
        assert_eq!(partial.total_pages, 2);

        let maximum = PagedResponse::<()>::new(Vec::new(), pagination, u64::MAX);
        assert_eq!(maximum.total_pages, u64::MAX / 20 + 1);
    }
}
