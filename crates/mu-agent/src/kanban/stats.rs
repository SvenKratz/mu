use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::document::DocumentState;
use super::state::KanbanState;
use crate::MuAgentError;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KanbanStats {
    pub total_documents: usize,
    pub todo: usize,
    pub processing: usize,
    pub feedback: usize,
    pub complete: usize,
    pub refining: usize,
    pub errored: usize,
    pub total_refines: u32,
    pub recent_activity: Vec<String>,
}

impl KanbanStats {
    pub fn from_state(state: &KanbanState) -> Self {
        let mut stats = Self {
            total_documents: state.documents.len(),
            ..Self::default()
        };
        for doc in state.documents.values() {
            match doc.state {
                DocumentState::Draft => {}
                DocumentState::Todo => stats.todo += 1,
                DocumentState::Processing => stats.processing += 1,
                DocumentState::Feedback => stats.feedback += 1,
                DocumentState::Complete => stats.complete += 1,
                DocumentState::Refining => stats.refining += 1,
                DocumentState::Error => stats.errored += 1,
            }
            stats.total_refines += doc.refine_count;
        }
        stats
    }

    pub fn log_activity(&mut self, message: impl Into<String>) {
        let entry = format!("[{}] {}", Utc::now().format("%H:%M:%S"), message.into());
        self.recent_activity.push(entry);
        // Keep last 20 activity entries
        if self.recent_activity.len() > 20 {
            self.recent_activity.remove(0);
        }
    }

    pub fn render_markdown(&self) -> String {
        let mut lines = Vec::new();
        lines.push("# Kanban Stats".to_string());
        lines.push(String::new());
        lines.push(format!("Last updated: {}", Utc::now().format("%Y-%m-%d %H:%M:%S UTC")));
        lines.push(String::new());
        lines.push("| Metric | Count |".to_string());
        lines.push("|--------|-------|".to_string());
        lines.push(format!("| Total documents | {} |", self.total_documents));
        lines.push(format!("| Todo | {} |", self.todo));
        lines.push(format!("| Processing | {} |", self.processing));
        lines.push(format!("| Awaiting feedback | {} |", self.feedback));
        lines.push(format!("| Complete | {} |", self.complete));
        lines.push(format!("| Refining | {} |", self.refining));
        lines.push(format!("| Total refines | {} |", self.total_refines));
        lines.push(String::new());

        if !self.recent_activity.is_empty() {
            lines.push("## Recent Activity".to_string());
            lines.push(String::new());
            for entry in &self.recent_activity {
                lines.push(format!("- {entry}"));
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    pub fn write_stats_file(&self, state: &KanbanState) -> Result<(), MuAgentError> {
        let stats_dir = state.stats_path();
        std::fs::create_dir_all(&stats_dir)?;
        let content = self.render_markdown();
        std::fs::write(stats_dir.join("STATS.md"), content)?;
        Ok(())
    }

    pub fn status_line(&self) -> String {
        format!(
            "todo={} proc={} fb={} done={}",
            self.todo, self.processing, self.feedback, self.complete
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::document::KanbanDocument;

    #[test]
    fn stats_from_state_counts_correctly() {
        let mut state = KanbanState::new(std::path::PathBuf::from("/tmp/test"));
        state.insert_document(KanbanDocument::new("a".to_string(), "task-a".to_string()));
        let mut doc_b = KanbanDocument::new("b".to_string(), "task-b".to_string());
        doc_b.state = DocumentState::Complete;
        state.insert_document(doc_b);

        let stats = KanbanStats::from_state(&state);
        assert_eq!(stats.total_documents, 2);
        assert_eq!(stats.todo, 1);
        assert_eq!(stats.complete, 1);
    }

    #[test]
    fn stats_renders_markdown() {
        let stats = KanbanStats {
            total_documents: 5,
            todo: 2,
            processing: 1,
            feedback: 0,
            complete: 2,
            refining: 0,
            errored: 0,
            total_refines: 1,
            recent_activity: vec!["[12:00:00] processed task-a".to_string()],
        };
        let md = stats.render_markdown();
        assert!(md.contains("# Kanban Stats"));
        assert!(md.contains("| Total documents | 5 |"));
        assert!(md.contains("processed task-a"));
    }

    #[test]
    fn status_line_format() {
        let stats = KanbanStats {
            todo: 3,
            processing: 1,
            feedback: 0,
            complete: 5,
            ..Default::default()
        };
        assert_eq!(stats.status_line(), "todo=3 proc=1 fb=0 done=5");
    }
}
