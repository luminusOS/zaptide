use std::cell::RefCell;

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub indexed_word_bytes: usize,
    pub raw_decoded: usize,
    pub decoded_bytes: usize,
    pub fragment_input: usize,
    pub fragment_output: usize,
    pub normalized_base_bytes: usize,
    pub normalized_tail_bytes: usize,
    pub normalized_revalidation_bytes: usize,
    pub url_indexed_bytes: usize,
    pub whatsapp_preprocessing: usize,
    pub whatsapp_authority_bytes: usize,
    pub whatsapp_authority_scanned: usize,
    pub whatsapp_parser_input_bytes: usize,
    pub whatsapp_parser_calls: usize,
    pub whatsapp_path_steps: usize,
    pub url_validation_bytes: usize,
    pub url_at_bytes: usize,
    pub url_path_steps: usize,
    pub url_queries: usize,
    pub url_opaque_parser_bytes: usize,
    pub url_special_bytes: usize,
    pub provider_prepare_calls: usize,
    pub max_prepares_per_decision: usize,
    pub provider_operations: usize,
    pub provider_segment_probes: usize,
    pub whatsapp_query_bytes: usize,
    pub provider_index_bytes: usize,
    pub provider_local_bytes: usize,
    pub max_provider_shared_capacity: usize,
    pub max_provider_local_capacity: usize,
    pub candidates: usize,
    pub accepted_parser_bytes: usize,
    pub consumed_web_bytes: usize,
    pub consumed_web_links: usize,
    pub output_bytes: usize,
}
thread_local! { static CURRENT: RefCell<Stats> = RefCell::new(Stats::default()); }
pub fn reset() {
    CURRENT.with(|s| *s.borrow_mut() = Stats::default());
}
pub fn snapshot() -> Stats {
    CURRENT.with(|s| s.borrow().clone())
}
pub(crate) fn update(f: impl FnOnce(&mut Stats)) {
    CURRENT.with(|s| f(&mut s.borrow_mut()));
}
