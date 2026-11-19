//! Read-only schema summary of a local file: tables, occurrences with
//! their relationships-graph boxes, and relationships, decoded as Manage
//! Database shows them (schema-manager.md).
//! `cargo run -p om-provider --example schema_catalog -- <file.fmp12>`
use om_hbam::HbamFile;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = HbamFile::open(std::env::args_os().nth(1).ok_or("file required")?)?;
    let tree = file.key_tree()?;
    let schema = om_provider::schema_from_tree(&tree)?;
    println!(
        "{} tables; {} occurrences; {} relationships (complete: {})",
        schema.tables.len(),
        schema.occurrences.len(),
        schema.relationships.len(),
        schema.relationships_decoded
    );
    for table in &schema.tables {
        println!("{}: {} fields", table.name, table.fields.len());
    }
    for occurrence in &schema.occurrences {
        match occurrence.graph {
            Some(graph) => println!(
                "{}: box {} {} {} x {}, list {}, display {} ({:?}), colour {:02x?}",
                occurrence.name,
                graph.x,
                graph.y,
                graph.width,
                graph.height,
                graph.list_height,
                graph.display,
                graph.shows(),
                graph.color
            ),
            None => println!("{}: no graph box read", occurrence.name),
        }
    }
    for relationship in &schema.relationships {
        let left = schema
            .occurrences
            .iter()
            .find(|o| o.id == relationship.left_occurrence)
            .ok_or("missing left occurrence")?;
        let right = schema
            .occurrences
            .iter()
            .find(|o| o.id == relationship.right_occurrence)
            .ok_or("missing right occurrence")?;
        for join in &relationship.matches {
            let left_field = schema
                .table(left.table_id.ok_or("missing left table")?)
                .and_then(|t| t.fields.iter().find(|f| f.id == join.left_field))
                .ok_or("missing left field")?;
            let right_field = schema
                .table(right.table_id.ok_or("missing right table")?)
                .and_then(|t| t.fields.iter().find(|f| f.id == join.right_field))
                .ok_or("missing right field")?;
            println!(
                "{}::{} {} {}::{}",
                left.name, left_field.name, join.comparison, right.name, right_field.name
            );
        }
    }
    Ok(())
}
