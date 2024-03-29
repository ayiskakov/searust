use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::result::Result;

use super::lexer::Lexer;

use serde::{Deserialize, Serialize};

pub trait Model {
    fn search_query(&self, query: &[char]) -> Result<Vec<(PathBuf, f32)>, ()>;
    fn add_document(&mut self, file_path: PathBuf, content: &[char]) -> Result<(), ()>;
}

pub type DocFreq = HashMap<String, usize>;
pub type TermFreq = HashMap::<String, usize>;

#[derive(Default, Deserialize, Serialize)]
struct Doc {
    tf: TermFreq,
    count: usize,
}
type Docs = HashMap<PathBuf, Doc>;

fn compute_tf(t: &str, doc: &Doc) -> f32 {
    let n = doc.count as f32;
    let m = doc.tf.get(t).cloned().unwrap_or(0) as f32;
    m / n
}

fn compute_idf(t: &str, n: usize, df: &DocFreq) -> f32 {
    let n = n as f32;
    let m = df.get(t).cloned().unwrap_or(1) as f32;
    (n / m).log10()
}


#[derive(Default, Deserialize, Serialize)]
pub struct InMemoryModel {
    docs: Docs,
    pub df: DocFreq,
}

impl Model for InMemoryModel {
    fn search_query(&self, query: &[char]) -> Result<Vec<(PathBuf, f32)>, ()> {
        let mut result = Vec::<(PathBuf, f32)>::new();
        let tokens = Lexer::new(&query).collect::<Vec<_>>();
        for (path, doc) in &self.docs {
            let mut rank = 0f32;
            for token in &tokens {
                rank += compute_tf(token, doc) * compute_idf(&token, self.docs.len(), &self.df);
            }
            result.push((path.clone(), rank));
        }
        result.sort_by(|(_, rank1), (_, rank2)| rank1.partial_cmp(rank2).unwrap());
        result.reverse();
        Ok(result)
    }

    fn add_document(&mut self, file_path: PathBuf, content: &[char]) -> Result<(), ()> {
        let mut tf = TermFreq::new();

        let mut count = 0;
        for term in Lexer::new(content) {
            if let Some(freq) = tf.get_mut(&term) {
                *freq += 1;
            } else {
                tf.insert(term, 1);
            }
            count += 1;
        }

        for t in tf.keys() {
            if let Some(freq) = self.df.get_mut(t) {
                *freq += 1;
            } else {
                self.df.insert(t.to_string(), 1);
            }
        }

        self.docs.insert(file_path, Doc {count, tf});
        Ok(())
    }
}

pub struct SqliteModel {
    connection: sqlite::Connection,
}

impl SqliteModel {
    fn execute(&self, statement: &str) -> Result<(), ()> {
        self.connection.execute(statement).map_err(|err| {
            eprintln!("ERROR: could not execute query {statement}: {err}")
        })?;
        Ok(())
    }

    pub fn begin(&self) -> Result<(), ()> {
        self.execute("BEGIN;")
    }

    pub fn commit(&self) -> Result<(), ()> {
        self.execute("COMMIT;")
    }

    fn migrate(&self) -> Result<(), ()>{
        self.execute("
             CREATE TABLE IF NOT EXISTS documents (
                 id INTEGER NOT NULL PRIMARY KEY,
                 path TEXT NOT NULL UNIQUE,
                 term_count INTEGER NOT NULL
             );
         ")?;

         self.execute("
             CREATE TABLE IF NOT EXISTS term_freq (
                 term TEXT NOT NULL,
                 doc_id INTEGER NOT NULL,
                 freq INTEGER NOT NULL,
                 UNIQUE(term, doc_id),
                 FOREIGN KEY(doc_id) REFERENCES documents(id)
             );
        ")?;

         self.execute("
             CREATE TABLE IF NOT EXISTS doc_freq (
                 term TEXT NOT NULL UNIQUE,
                 freq INTEGER
             );
         ")?;

         Ok(())
    }

    pub fn open(path: &Path) -> Result<Self, ()> {
        let connection = sqlite::open(path).map_err(|err| {
            eprintln!("ERROR: could not open sqlite database {path}: {err}", path = path.display())
        })?;

        let this = Self {connection};

        this.migrate().map_err(|err| {
            eprintln!("ERROR: error occured during migration {err:?})");
        })?;

        Ok(this)
    }
}


impl Model for SqliteModel {
    fn search_query(&self, query: &[char]) -> Result<Vec<(PathBuf, f32)>, ()> {
        todo!()
    }

    fn add_document(&mut self, file_path: PathBuf, content: &[char]) -> Result<(), ()> {
        let terms = Lexer::new(content).collect::<Vec<_>>();

        let doc_id = {
            let query = "INSERT INTO document (path, term_count) VALUES (:path, :count) RETURNING id";
            let log_err = |err| {
                eprintln!("ERROR: could not prepare or execute query {query}: {err}")
            };
            let mut stmt = self.connection.prepare(query).map_err(log_err)?;

            stmt.bind_iter::<_, (_, sqlite::Value)>([
                (":path", file_path.display().to_string().as_str().into()),
                (":count", (terms.len() as i64).into()),
            ]).map_err(log_err)?;

            stmt.next().map_err(log_err)?;

            match stmt.next().map_err(log_err)? {
                sqlite::State::Row => stmt.read::<i64, _>("id").map_err(log_err)?,
                sqlite::State::Done => 0
            }
        };

        let mut tf = TermFreq::new();
        for term in Lexer::new(content) {
            if let Some(freq) = tf.get_mut(&term) {
                *freq += 1;
            } else {
                tf.insert(term, 1);
            }
        }

        for (term, freq) in &tf {
            {
                let query = "INSERT INTO term_freq(doc_id, term, freq) VALUES (:doc_id, :term, :freq)";
                let log_err = |err| {
                    eprintln!("ERROR: could not execute or prepare query {query}: {err}");
                };
                let mut stmt = self.connection.prepare(query).map_err(log_err)?;
                stmt.bind_iter::<_, (_, sqlite::Value)>([
                    (":doc_id", doc_id.into()),
                    (":term", term.as_str().into()),
                    (":freq", (*freq as i64).into()),
                ]).map_err(log_err)?;
                stmt.next().map_err(log_err)?;
            }

            {
                let freq = {
                    let query = "SELECT freq FROM doc_freq WHERE term = :term";
                    let log_err = |err| {
                        eprintln!("ERROR: could not prepare or execute query {query}: {err}");
                    };
                    let mut stmt = self.connection.prepare(query).map_err(log_err)?;
                    stmt.bind_iter::<_, (_, sqlite::Value)>([
                        (":term", term.as_str().into()),
                    ]).map_err(log_err)?;
                    match stmt.next().map_err(log_err)? {
                        sqlite::State::Row => stmt.read::<i64, _>("freq").map_err(log_err)?,
                        sqlite::State::Done => 0
                    }
                };

                // TODO: find a better way to auto increment the frequency
                let query = "INSERT OR REPLACE INTO doc_freq(term, freq) VALUES (:term, :freq)";
                let log_err = |err| {
                    eprintln!("ERROR: could not execute or prepare query {query}: {err}");
                };
                let mut stmt = self.connection.prepare(query).map_err(log_err)?;
                stmt.bind_iter::<_, (_, sqlite::Value)>([
                    (":term", term.as_str().into()),
                    (":freq", (freq + 1).into()),
                ]).map_err(log_err)?;
                stmt.next().map_err(log_err)?;
            }
        }
        
        Ok(())
    }
}