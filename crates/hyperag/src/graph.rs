//! The relationship graph in Neo4j over its HTTP Query API (a port of `src/adapters/neo4j.ts`).
//! Nodes are memory ids with their scope; edges are typed and carry who made them; no text.
//! Every read takes the caller's readable scopes and keeps only paths whose every node is readable.

use std::time::Duration;

use serde_json::{Value, json};

use crate::types::{Error, Relation, Result};

#[derive(Clone)]
pub struct Graph {
    http: reqwest::Client,
    url: String,
    user: String,
    password: String,
}

pub struct Expanded {
    pub id: String,
    pub paths: f64,
    pub via: Vec<String>,
}

impl Graph {
    pub fn new(base_url: &str, user: &str, password: &str, database: Option<&str>) -> Self {
        Graph {
            http: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("http client"),
            url: format!("{}/db/{}/query/v2", base_url.trim_end_matches('/'), database.unwrap_or("neo4j")),
            user: user.into(),
            password: password.into(),
        }
    }

    async fn run(&self, statement: &str, parameters: Value) -> Result<Vec<Vec<Value>>> {
        let unavailable = |why: String| Error::Dependency(format!("graph_unavailable: {why}"));
        let response = self
            .http
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({ "statement": statement, "parameters": parameters }))
            .send()
            .await
            .map_err(|_| unavailable("unreachable".into()))?;
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let values = body.pointer("/data/values").and_then(Value::as_array);
        match values {
            Some(rows) if status == 200 || status == 202 => {
                Ok(rows.iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect())
            }
            _ => Err(unavailable(
                body.pointer("/errors/0/code").and_then(Value::as_str).map(str::to_owned).unwrap_or(format!("http_{status}")),
            )),
        }
    }

    pub async fn ensure_schema(&self) -> Result<()> {
        self.run("CREATE CONSTRAINT hyperag_memory_id IF NOT EXISTS FOR (m:Memory) REQUIRE m.id IS UNIQUE", json!({})).await?;
        Ok(())
    }

    pub async fn upsert_memory(&self, id: &str, scope: &str) -> Result<()> {
        self.run("MERGE (m:Memory {id: $id}) SET m.scope = $scope", json!({ "id": id, "scope": scope })).await?;
        Ok(())
    }

    /// MERGE on (from, to, type): relating twice is a no-op, so the write is idempotent.
    pub async fn relate(&self, rel: &Relation, from_scope: &str, to_scope: &str) -> Result<()> {
        self.run(
            "MERGE (a:Memory {id: $from}) SET a.scope = $fromScope
             MERGE (b:Memory {id: $to}) SET b.scope = $toScope
             MERGE (a)-[r:REL {type: $type}]->(b)
             ON CREATE SET r.createdBy = $createdBy, r.createdAt = $createdAt",
            json!({ "from": rel.from, "to": rel.to, "type": rel.kind, "createdBy": rel.created_by,
                    "createdAt": rel.created_at, "fromScope": from_scope, "toScope": to_scope }),
        )
        .await?;
        Ok(())
    }

    pub async fn expand(&self, seeds: &[String], readable: &[String], hops: usize, limit: i64) -> Result<Vec<Expanded>> {
        if seeds.is_empty() || readable.is_empty() {
            return Ok(vec![]);
        }
        let depth = hops.clamp(1, 3);
        let rows = self
            .run(
                &format!(
                    "MATCH p = (s:Memory)-[:REL*1..{depth}]-(n:Memory)
                     WHERE s.id IN $seeds AND NOT n.id IN $seeds AND all(x IN nodes(p) WHERE x.scope IN $readable)
                     WITH n, count(p) AS paths, collect(DISTINCT s.id) AS via
                     RETURN n.id, paths, via[0..5] ORDER BY paths DESC, n.id LIMIT $limit"
                ),
                json!({ "seeds": seeds, "readable": readable, "limit": limit }),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| Expanded {
                id: r.first().and_then(Value::as_str).unwrap_or_default().to_owned(),
                paths: r.get(1).and_then(Value::as_f64).unwrap_or(0.0),
                via: r.get(2).and_then(Value::as_array).map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
            })
            .collect())
    }

    pub async fn relations(&self, id: &str, readable: &[String]) -> Result<Vec<Relation>> {
        let rows = self
            .run(
                "MATCH (a:Memory {id: $id})-[r:REL]-(b:Memory)
                 WHERE a.scope IN $readable AND b.scope IN $readable
                 RETURN startNode(r).id, endNode(r).id, r.type, r.createdBy, r.createdAt
                 ORDER BY r.createdAt, startNode(r).id, endNode(r).id",
                json!({ "id": id, "readable": readable }),
            )
            .await?;
        let s = |r: &Vec<Value>, i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
        Ok(rows
            .iter()
            .map(|r| Relation { from: s(r, 0), to: s(r, 1), kind: s(r, 2), created_by: s(r, 3), created_at: s(r, 4) })
            .collect())
    }

    /// Every edge with its endpoints' scopes, in a stable order, one page at a time (for export).
    pub async fn all_relations(&self, skip: i64, limit: i64) -> Result<Vec<(Relation, String, String)>> {
        let rows = self
            .run(
                "MATCH (a:Memory)-[r:REL]->(b:Memory)
                 RETURN a.id, b.id, r.type, r.createdBy, r.createdAt, a.scope, b.scope
                 ORDER BY a.id, b.id, r.type SKIP $skip LIMIT $limit",
                json!({ "skip": skip, "limit": limit }),
            )
            .await?;
        let s = |r: &Vec<Value>, i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
        Ok(rows
            .iter()
            .map(|r| (Relation { from: s(r, 0), to: s(r, 1), kind: s(r, 2), created_by: s(r, 3), created_at: s(r, 4) }, s(r, 5), s(r, 6)))
            .collect())
    }

    pub async fn delete_memory(&self, id: &str) -> Result<()> {
        self.run("MATCH (m:Memory {id: $id}) DETACH DELETE m", json!({ "id": id })).await?;
        Ok(())
    }
}
