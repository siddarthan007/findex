use crate::storage::Symbol;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{QueryParser, RegexQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Term, TextFieldIndexing, TextOptions, Value, STORED, STRING,
    TEXT,
};
use tantivy::tokenizer::{LowerCaser, NgramTokenizer, TextAnalyzer};
use tantivy::{doc, Index, IndexWriter, ReloadPolicy};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LexicalError {
    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("Query parser error: {0}")]
    QueryParser(#[from] tantivy::query::QueryParserError),
    #[error("Open directory error: {0}")]
    OpenDirectory(#[from] tantivy::directory::error::OpenDirectoryError),
}

pub struct LexicalIndex {
    index: Index,
    _schema: Schema,
    id_field: Field,
    name_field: Field,
    name_ngram_field: Field,
    name_raw_field: Field,
    body_field: Field,
}

impl LexicalIndex {
    fn schema() -> (Schema, Field, Field, Field, Field, Field) {
        let ngram_options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("identifier_trigram")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );

        let mut schema_builder = Schema::builder();
        let id_field = schema_builder.add_text_field("id", STRING | STORED);
        let name_field = schema_builder.add_text_field("name", TEXT | STORED);
        let name_ngram_field = schema_builder.add_text_field("name_ngrams", ngram_options);
        let name_raw_field = schema_builder.add_text_field("name_raw", STRING | STORED);
        let body_field = schema_builder.add_text_field("body", TEXT | STORED);
        (
            schema_builder.build(),
            id_field,
            name_field,
            name_ngram_field,
            name_raw_field,
            body_field,
        )
    }

    fn register_tokenizers(index: &Index) -> Result<(), LexicalError> {
        let analyzer = TextAnalyzer::builder(NgramTokenizer::new(3, 3, false)?)
            .filter(LowerCaser)
            .build();
        index.tokenizers().register("identifier_trigram", analyzer);
        Ok(())
    }

    pub fn open_or_create<P: AsRef<Path>>(dir_path: P) -> Result<Self, LexicalError> {
        let (schema, id_field, name_field, name_ngram_field, name_raw_field, body_field) =
            Self::schema();

        let index_path = dir_path.as_ref();
        if !index_path.exists() {
            std::fs::create_dir_all(index_path).ok();
        }

        let directory = tantivy::directory::MmapDirectory::open(index_path)?;
        let index = match Index::open_or_create(directory, schema.clone()) {
            Ok(index) => index,
            Err(tantivy::TantivyError::SchemaError(_)) => {
                // Schema v2 adds trigram/raw identifier fields. Tantivy can
                // safely recreate only this managed index directory; callers
                // detect the empty index and rebuild it from primary storage.
                Index::create(
                    tantivy::directory::MmapDirectory::open(index_path)?,
                    schema.clone(),
                    tantivy::IndexSettings::default(),
                )?
            }
            Err(error) => return Err(error.into()),
        };
        Self::register_tokenizers(&index)?;

        Ok(Self {
            index,
            _schema: schema,
            id_field,
            name_field,
            name_ngram_field,
            name_raw_field,
            body_field,
        })
    }

    pub fn create_in_ram() -> Result<Self, LexicalError> {
        let (schema, id_field, name_field, name_ngram_field, name_raw_field, body_field) =
            Self::schema();

        let index = Index::create_in_ram(schema.clone());
        Self::register_tokenizers(&index)?;

        Ok(Self {
            index,
            _schema: schema,
            id_field,
            name_field,
            name_ngram_field,
            name_raw_field,
            body_field,
        })
    }

    /// Indexes a list of symbols. Clears previous documents to prevent duplicates.
    pub fn index_symbols(
        &self,
        symbols: &[Symbol],
        symbol_bodies: &[String],
    ) -> Result<(), LexicalError> {
        // Limit index writer buffer size to 50MB to stay within memory limits
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        writer.delete_all_documents()?;

        for (sym, body) in symbols.iter().zip(symbol_bodies.iter()) {
            writer.add_document(doc!(
                self.id_field => sym.id.clone(),
                self.name_field => sym.name.clone(),
                self.name_ngram_field => sym.name.clone(),
                self.name_raw_field => sym.name.to_lowercase(),
                self.body_field => body.clone()
            ))?;
        }

        writer.commit()?;
        Ok(())
    }

    /// Incrementally updates the lexical index: deletes documents for removed or modified symbols,
    /// then adds documents for newly parsed symbols.
    pub fn update_symbols(
        &self,
        added: &[(Symbol, String)],
        deleted_ids: &[String],
    ) -> Result<(), LexicalError> {
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;

        for id in deleted_ids {
            let term = Term::from_field_text(self.id_field, id);
            writer.delete_term(term);
        }

        for (sym, body) in added {
            writer.add_document(doc!(
                self.id_field => sym.id.clone(),
                self.name_field => sym.name.clone(),
                self.name_ngram_field => sym.name.clone(),
                self.name_raw_field => sym.name.to_lowercase(),
                self.body_field => body.clone()
            ))?;
        }

        writer.commit()?;
        Ok(())
    }

    /// Searches the lexical index using BM25 and returns a list of matching symbol IDs and their scores.
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>, LexicalError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        let searcher = reader.searcher();
        if let Some(pattern) = query_str.strip_prefix("regex:") {
            return self.search_regex(pattern.trim(), limit);
        }

        let mut query_parser = QueryParser::for_index(
            &self.index,
            vec![self.name_field, self.name_ngram_field, self.body_field],
        );
        query_parser.set_field_boost(self.name_field, 3.0);
        query_parser.set_field_boost(self.name_ngram_field, 1.5);
        let query = query_parser.parse_query(query_str)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;
        let mut results = Vec::new();

        for (score, doc_address) in top_docs {
            let doc = searcher.doc::<tantivy::TantivyDocument>(doc_address)?;
            if let Some(id_value) = doc.get_first(self.id_field) {
                if let Some(id) = id_value.as_str() {
                    results.push((id.to_string(), score));
                }
            }
        }

        Ok(results)
    }

    /// Regex identifier search. Prefix a normal lexical query with `regex:`
    /// to route to this method (for example `regex:^parse_.*`).
    pub fn search_regex(
        &self,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>, LexicalError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let searcher = reader.searcher();
        let query = RegexQuery::from_pattern(&pattern.to_lowercase(), self.name_raw_field)?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc = searcher.doc::<tantivy::TantivyDocument>(doc_address)?;
            if let Some(id) = doc
                .get_first(self.id_field)
                .and_then(|value| value.as_str())
            {
                results.push((id.to_string(), score));
            }
        }
        Ok(results)
    }

    pub fn num_docs(&self) -> Result<u64, LexicalError> {
        let reader = self.index.reader()?;
        Ok(reader.searcher().num_docs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexical_search() {
        let lexical = LexicalIndex::create_in_ram().unwrap();

        let syms = vec![
            Symbol {
                id: "src/main.rs#main".to_string(),
                name: "main".to_string(),
                kind: "Function".to_string(),
                signature: "fn main()".to_string(),
                file_path: "src/main.rs".to_string(),
                start_line: 1,
                start_col: 1,
                end_line: 3,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
            Symbol {
                id: "src/helper.rs#add".to_string(),
                name: "add".to_string(),
                kind: "Function".to_string(),
                signature: "fn add(a: i32, b: i32) -> i32".to_string(),
                file_path: "src/helper.rs".to_string(),
                start_line: 1,
                start_col: 1,
                end_line: 3,
                end_col: 1,
                docstring: None,
                ..Default::default()
            },
        ];

        let bodies = vec![
            "fn main() {\n  println!(\"Hello\");\n}".to_string(),
            "fn add(a: i32, b: i32) -> i32 {\n  a + b\n}".to_string(),
        ];

        lexical.index_symbols(&syms, &bodies).unwrap();

        let results = lexical.search("main", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "src/main.rs#main");

        let results2 = lexical.search("add", 10).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].0, "src/helper.rs#add");

        let fuzzy = lexical.search("calclate_sum", 10).unwrap();
        // No calculate_sum fixture yet, but a fuzzy identifier query must be
        // accepted by the trigram field rather than failing to parse.
        assert!(fuzzy.is_empty());

        // Tantivy regexes are implicitly anchored to the whole term; explicit
        // zero-width anchors are rejected by its finite-state automaton.
        let regex = lexical.search("regex:a.+", 10).unwrap();
        assert_eq!(regex[0].0, "src/helper.rs#add");
    }
}
