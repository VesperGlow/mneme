use crate::{config::Config, domain::*, llm::bounded_body};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct Graph {
    client: reqwest::Client,
    url: String,
    username: String,
    password: String,
    pub metrics: std::sync::Arc<crate::telemetry::Metrics>,
}

impl Graph {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            url: format!(
                "{}/db/{}/tx/commit",
                config.neo4j.base_url.trim_end_matches('/'),
                config.neo4j.database
            ),
            username: config.neo4j.username.clone(),
            password: config.neo4j_password.clone(),
            metrics: Default::default(),
        })
    }
    pub async fn query(&self, cypher: &str, parameters: Value) -> Result<Vec<Vec<Value>>> {
        let start = std::time::Instant::now();
        let result = self.query_inner(cypher, parameters).await;
        self.metrics.record(start, result.is_ok());
        result
    }
    async fn query_inner(&self, cypher: &str, parameters: Value) -> Result<Vec<Vec<Value>>> {
        let response = self.client.post(&self.url).basic_auth(&self.username,Some(&self.password))
            .json(&json!({"statements":[{"statement":cypher,"parameters":parameters,"resultDataContents":["row"]}]}))
            .send().await?.error_for_status()?;
        let value: Value = serde_json::from_slice(&bounded_body(response, 4_194_304).await?)?;
        ensure!(
            value["errors"].as_array().is_some_and(|a| a.is_empty()),
            "Neo4j query rejected"
        );
        let data = value["results"][0]["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid graph response"))?;
        data.iter()
            .map(|r| {
                r["row"]
                    .as_array()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid graph row"))
            })
            .collect()
    }
    pub async fn health(&self) -> bool {
        self.query("RETURN 1", json!({})).await.is_ok()
    }
    pub async fn initialize(&self) -> Result<()> {
        for statement in [
            "CREATE CONSTRAINT memory_id IF NOT EXISTS FOR (m:Memory) REQUIRE m.id IS UNIQUE",
            "CREATE CONSTRAINT entity_name IF NOT EXISTS FOR (e:Entity) REQUIRE e.name IS UNIQUE",
            "CREATE VECTOR INDEX memory_vector IF NOT EXISTS FOR (m:ActiveMemory) ON m.embedding OPTIONS {indexConfig: {`vector.dimensions`:1024,`vector.similarity_function`:'cosine'}}",
            "CREATE FULLTEXT INDEX memory_text IF NOT EXISTS FOR (m:ActiveMemory) ON EACH [m.content] OPTIONS {indexConfig: {`fulltext.analyzer`:'cjk'}}",
        ] {
            self.query(statement, json!({})).await?;
        }
        self.query("CALL db.awaitIndexes(60)", json!({})).await?;
        Ok(())
    }

    pub async fn project(&self, memory: &Memory) -> Result<()> {
        if !memory.available(now()) {
            self.query(
                "MATCH (m:Memory {id:$id}) OPTIONAL MATCH (m)-[:MENTIONS]->(e:Entity) WITH m,collect(e) AS entities DETACH DELETE m WITH entities UNWIND entities AS e WITH e WHERE NOT (e)--() DELETE e",
                json!({"id":memory.id}),
            )
            .await?;
            return Ok(());
        }
        let entities: Vec<String> = serde_json::from_str(&memory.entities)?;
        let embedding: Vec<f32> = serde_json::from_str(&memory.embedding)?;
        // Remove stale entity edges on a retry/update; idempotent MERGE avoids duplicates.
        self.query("MERGE (m:Memory {id:$id}) SET m:ActiveMemory, m.content=$content,m.embedding=$embedding,m.slot=$slot,m.revision=$revision,m.updated_at=$updated_at,m.created_at=$created_at,m.source_seq=$source_seq,m.tier=$tier,m.evidence_type=$evidence_type,m.confidence=$confidence,m.valid_until=$valid_until WITH m OPTIONAL MATCH (m)-[r:MENTIONS]->(previous:Entity) WITH m,collect(r) AS edges,collect(previous) AS old_entities FOREACH (edge IN edges | DELETE edge) FOREACH (name IN $entities | MERGE (e:Entity {name:name}) MERGE (m)-[:MENTIONS]->(e)) WITH m,old_entities CALL { WITH old_entities UNWIND old_entities AS old WITH old WHERE NOT (old)--() DELETE old RETURN count(*) AS removed } RETURN m.id",
            json!({"id":memory.id,"content":memory.content,"embedding":embedding,"slot":memory.slot,"revision":memory.revision,"updated_at":memory.updated_at,"created_at":memory.created_at,"source_seq":memory.source_seq,"tier":memory.tier,"evidence_type":memory.evidence_type,"confidence":memory.confidence,"valid_until":memory.valid_until,"entities":entities})).await?;
        Ok(())
    }
    pub async fn vector(&self, vector: &[f32], limit: usize) -> Result<Vec<String>> {
        Ok(self
            .vector_hits(vector, limit)
            .await?
            .into_iter()
            .map(|hit| hit.id)
            .collect())
    }
    pub async fn vector_hits(
        &self,
        vector: &[f32],
        limit: usize,
    ) -> Result<Vec<crate::memory::SearchHit>> {
        scored(self.query("CALL db.index.vector.queryNodes('memory_vector',$limit,$vector) YIELD node,score RETURN node.id,score ORDER BY score DESC",json!({"limit":limit,"vector":vector})).await?)
    }
    pub async fn keyword(&self, text: &str, limit: usize) -> Result<Vec<String>> {
        Ok(self
            .keyword_hits(text, limit)
            .await?
            .into_iter()
            .map(|hit| hit.id)
            .collect())
    }
    pub async fn keyword_hits(
        &self,
        text: &str,
        limit: usize,
    ) -> Result<Vec<crate::memory::SearchHit>> {
        let query = lucene_query(text);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        scored(self.query("CALL db.index.fulltext.queryNodes('memory_text',$query,{limit:$limit}) YIELD node,score RETURN node.id,score ORDER BY score DESC",json!({"query":query,"limit":limit})).await?)
    }
    pub async fn expand(&self, text: &str, limit: usize) -> Result<Vec<String>> {
        // Shared entities form a bounded two-hop graph; entity names can also be mentioned directly.
        ids(self.query("MATCH (n:ActiveMemory)-[:MENTIONS]->(e:Entity) WHERE e.name <> '' AND size(e.name)>1 AND toLower($text) CONTAINS toLower(e.name) WITH n,count(DISTINCT e) AS weight RETURN n.id ORDER BY weight DESC,n.updated_at DESC LIMIT $limit",json!({"text":text,"limit":limit})).await?)
    }
    pub async fn neighbors(&self, seeds: &[String], limit: usize) -> Result<Vec<String>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        // The uniqueness constraint is on Memory.id. Include that label and
        // seek each seed instead of scanning the whole ActiveMemory label.
        ids(self.query("UNWIND $seeds AS seed MATCH (m:Memory:ActiveMemory {id:seed})-[:MENTIONS]->(e:Entity)<-[:MENTIONS]-(n:ActiveMemory) WHERE NOT n.id IN $seeds WITH n,count(DISTINCT e) AS weight RETURN n.id ORDER BY weight DESC,n.updated_at DESC LIMIT $limit",json!({"seeds":seeds,"limit":limit})).await?)
    }
}

fn scored(rows: Vec<Vec<Value>>) -> Result<Vec<crate::memory::SearchHit>> {
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            Ok(crate::memory::SearchHit {
                id: row
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("invalid graph row"))?
                    .into(),
                rank: i + 1,
                score: row.get(1).and_then(Value::as_f64),
                content: None,
                excluded: false,
            })
        })
        .collect()
}

fn ids(rows: Vec<Vec<Value>>) -> Result<Vec<String>> {
    rows.into_iter()
        .map(|r| {
            r.first()
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid memory ID"))
        })
        .collect()
}

/// Quote alphanumeric runs. Chinese queries are split into overlapping bigrams to preserve
/// partial recall with Lucene's CJK analyzer, without accepting Lucene query syntax from users.
pub fn lucene_query(text: &str) -> String {
    let mut terms = std::collections::BTreeSet::new();
    let mut chinese = Vec::new();
    let mut word = String::new();
    let flush = |chinese: &mut Vec<char>,
                 word: &mut String,
                 terms: &mut std::collections::BTreeSet<String>| {
        if chinese.len() == 1 {
            terms.insert(chinese[0].to_string());
        }
        for pair in chinese.windows(2) {
            terms.insert(pair.iter().collect());
        }
        chinese.clear();
        if !word.is_empty() {
            terms.insert(std::mem::take(word));
        }
    };
    for c in text.chars().take(512) {
        if ('\u{3400}'..='\u{9fff}').contains(&c) {
            if !word.is_empty() {
                terms.insert(std::mem::take(&mut word));
            }
            chinese.push(c);
        } else if c.is_alphanumeric() {
            flush(&mut chinese, &mut String::new(), &mut terms);
            word.extend(c.to_lowercase());
        } else {
            flush(&mut chinese, &mut word, &mut terms);
        }
    }
    flush(&mut chinese, &mut word, &mut terms);
    terms
        .into_iter()
        .take(64)
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}
