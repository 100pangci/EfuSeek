#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortField {
    #[default]
    Default,
    Name,
    Size,
    Modified,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sort {
    pub field: SortField,
    pub direction: SortDirection,
}
