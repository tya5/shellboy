//! Test-only access to the production stream sealing step. No product state.
pub(crate) fn seal(node: &mut crate::ui::UiNode) {
    super::streams::seal_node(node);
}
