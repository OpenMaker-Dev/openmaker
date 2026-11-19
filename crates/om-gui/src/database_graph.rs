//! Manage Database's relationships graph (the Relationships tab), drawn as
//! FileMaker Pro 26 draws it. Each table occurrence sits at
//! the box its definition stores (schema-path.md 5.3): a title bar in its
//! colour, the key fields (the fields its relationships match on), then
//! every field of its table. Each relationship runs from key field to key
//! field, with its operator on the line and a crow's foot on the "many"
//! side. Under the graph are FileMaker's tool groups. Where the schema can
//! be saved, a box or a relationship is selected by a click and edited by a
//! double click, a box is moved by dragging it, and the Tables /
//! Relationships tools add, edit, delete and colour; the graph answers with
//! [`GraphEvent`]s, which Manage Database turns into schema changes.
//!
//! Sizes are points at 100%, measured on FileMaker Pro 26's drawing of the
//! Contacts starter solution (schema-path.md 5.3); schema-manager.md says
//! what is measured and what is OpenMaker's choice.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

use egui::text::LayoutJob;
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Mesh, Painter, Pos2, Rect, Response,
    RichText, Role, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use om_data::{
    DatabaseSchema, FieldKind, OccurrenceBox, OccurrenceShows, SchemaField, SchemaFieldKind,
    SchemaOccurrence, SchemaRelationship, SchemaTable,
};

use crate::icons::{self, Icon};
use crate::{theme, widgets};

/// The sentence FileMaker shows above the graph.
const INTRO: &str = "The relationships graph provides access to data in one table from \
                     another. If a relationship is defined between two tables (even through \
                     another table), fields from one table can be accessed from the other.";

/// Title bar height.
const TITLE: f32 = 18.0;
/// Field row pitch, in the key-field area and the field list.
const ROW: f32 = 12.0;
/// The frame at either side of a box, and at its bottom.
const SIDE: f32 = 4.0;
const FRAME: f32 = 5.0;
/// The band between the key-field area and the field list.
const BAND: f32 = 5.0;
/// Field names, from the box's left edge.
const TEXT_X: f32 = 13.0;
/// A key marker's center, from the edge its relationship leaves by.
const MARKER_X: f32 = 8.6;
const RADIUS: f32 = 3.0;
/// Inter at 10.5 points, 0.35 points tighter per letter, has FileMaker's
/// cap height (7.5 points) and the widths of its field names in the
/// Contacts starter solution within about 2 points.
const FONT: f32 = 10.5;
const LETTER_SPACING: f32 = -0.35;
/// The horizontal line a relationship leaves a key field by.
const STUB: f32 = 14.0;
/// A crow's foot's length and half its spread at the box.
const FOOT: f32 = 4.5;
const SPREAD: f32 = 4.25;
/// The framed operator on a relationship line.
const OPERATOR: Vec2 = vec2(16.0, 12.0);
/// Blank graph beyond the furthest box.
const MARGIN: f32 = 40.0;
/// Stored coordinates and sizes are clamped to this before drawing, so a
/// hostile definition cannot size the canvas.
const LIMIT: f32 = 20_000.0;
/// Key-field rows a box draws at most; with more key fields the last row
/// counts the rest, so every box stays well within [`LIMIT`] (OpenMaker's
/// choice).
const MAX_KEY_ROWS: usize = 200;

const ZOOM_STEPS: [u16; 8] = [25, 50, 75, 100, 150, 200, 300, 400];
const ZOOM_MIN: u16 = 10;
const ZOOM_MAX: u16 = 400;

/// Why an editing tool is unavailable.
const READ_ONLY: &str = "This schema cannot be changed here.";
const NOT_BUILT: &str = "OpenMaker does not have this tool yet.";

/// The colours the colour tool offers: FileMaker's default grey, then a
/// palette of OpenMaker's choosing (FileMaker Pro opens the system colour
/// panel).
const COLORS: [[u8; 4]; 8] = [
    [0x78, 0x78, 0x78, 0xff],
    [0x63, 0x8c, 0xc7, 0xff],
    [0x6a, 0xa8, 0x4f, 0xff],
    [0xd0, 0x6a, 0x5a, 0xff],
    [0xe0, 0x9a, 0x3e, 0xff],
    [0xd9, 0xc4, 0x46, 0xff],
    [0x9a, 0x6f, 0xb8, 0xff],
    [0x4f, 0xa8, 0xa6, 0xff],
];

/// What the user did in the graph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GraphEvent {
    SelectOccurrence(u64),
    /// Shift-click: add the occurrence to the selection, or take it out.
    ToggleOccurrence(u64),
    /// Duplicate the selected occurrences.
    Duplicate,
    /// An Arrange tool's choice for the selected occurrences (from the
    /// tools; [`show`] plans it as [`GraphEvent::ArrangeAll`]).
    ArrangeSelected(om_data::Arrangement),
    /// The selected boxes as an Arrange tool leaves them, or why not.
    ArrangeAll(Result<om_data::SchemaChange, String>),
    SelectRelationship(u64),
    /// A double click on a box (Specify Table).
    EditOccurrence(u64),
    /// A double click on a relationship (Edit Relationship).
    EditRelationship(u64),
    /// A box dragged to a new place, in graph points.
    Arrange {
        occurrence: u64,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
    },
    AddOccurrence,
    AddRelationship,
    LinkFields {
        from: (u64, u64),
        to: (u64, u64),
    },
    StaleFieldLink,
    EditSelected,
    DeleteSelected,
    Color([u8; 4]),
}

/// A gesture belongs to the schema it started on. A changed catalog cannot
/// silently substitute another occurrence/field with the same numeric ID.
#[derive(Clone)]
struct FieldDrag {
    from: (u64, u64),
    origin: Pos2,
    schema: std::sync::Arc<DatabaseSchema>,
}

/// What is selected in the graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) occurrence: Option<u64>,
    pub(crate) relationship: Option<u64>,
    /// The other occurrences selected with `occurrence` (Shift-click).
    pub(crate) also: std::collections::BTreeSet<u64>,
}

impl Selection {
    fn has(&self, occurrence: u64) -> bool {
        self.occurrence == Some(occurrence) || self.also.contains(&occurrence)
    }
}

/// Draw the Relationships tab's graph and tools. `editable` enables the
/// tools that change the schema.
pub(crate) fn show(
    ui: &mut Ui,
    schema: &DatabaseSchema,
    selection: &Selection,
    editable: bool,
    height: f32,
) -> Vec<GraphEvent> {
    let view_id = Id::unique("database_relationship_graph_view");
    let mut view: View = ui.data(|data| data.get_temp(view_id)).unwrap_or_default();
    ui.label(INTRO);
    ui.add_space(8.0);
    let mut graph = Graph::new(schema);
    // A box being dragged is drawn where it goes.
    if let Some((id, offset)) = view.drag {
        if let Some(node) = graph.nodes.iter_mut().find(|n| n.occurrence.id == id) {
            node.graph.x += offset.x;
            node.graph.y += offset.y;
        }
    }
    let mut events = Vec::new();
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, Color32::from_gray(0x40)))
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt("database_relationship_graph")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    graph.paint(ui, schema, selection, editable, &mut view, &mut events);
                });
        });
    if !schema.relationships_decoded {
        ui.label(
            RichText::new("Some relationship definitions could not be decoded for this file.")
                .small()
                .color(theme::CAPTION),
        );
    }
    ui.add_space(8.0);
    tools(ui, &mut view, editable, selection, &mut events);
    // The selected boxes where they are stored (the graph's own place for
    // an empty box), as the move of one box saves them.
    for event in &mut events {
        if let GraphEvent::ArrangeSelected(how) = *event {
            let frames: Vec<(u64, [f32; 4])> = graph
                .nodes
                .iter()
                .filter(|n| selection.has(n.occurrence.id))
                .map(|n| {
                    let b = &n.graph;
                    (n.occurrence.id, [b.x, b.y, b.width, b.height])
                })
                .collect();
            *event = GraphEvent::ArrangeAll(om_data::arrange(&frames, how).map_err(|e| e.message));
        }
    }
    ui.data_mut(|data| data.insert_temp(view_id, view));
    events
}

/// The zoom and a box being dragged, kept across frames and tabs.
#[derive(Clone)]
struct View {
    percent: u16,
    text: String,
    /// The occurrence being dragged and how far, in graph points.
    drag: Option<(u64, Vec2)>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            percent: 100,
            text: "100".into(),
            drag: None,
        }
    }
}

impl View {
    fn factor(&self) -> f32 {
        f32::from(self.percent) / 100.0
    }

    fn set(&mut self, percent: u16) {
        self.percent = percent.clamp(ZOOM_MIN, ZOOM_MAX);
        self.text = self.percent.to_string();
    }

    fn zoom_in(&mut self) {
        if let Some(&next) = ZOOM_STEPS.iter().find(|&&step| step > self.percent) {
            self.set(next);
        }
    }

    fn zoom_out(&mut self) {
        if let Some(&next) = ZOOM_STEPS.iter().rev().find(|&&step| step < self.percent) {
            self.set(next);
        }
    }
}

/// One table occurrence as drawn.
struct Node<'a> {
    occurrence: &'a SchemaOccurrence,
    fields: &'a [SchemaField],
    /// The key fields shown, in the table's field order: those its
    /// relationships match on, at most [`MAX_KEY_ROWS`] less the count row.
    keys: Vec<&'a SchemaField>,
    /// Key fields past [`MAX_KEY_ROWS`], counted in the last row.
    hidden_keys: usize,
    /// Every key field by id: its row (the count row for hidden ones) and
    /// the field.
    key_rows: BTreeMap<u64, (usize, &'a SchemaField)>,
    graph: OccurrenceBox,
    /// The stored box, or OpenMaker's placement when none was read.
    stored: bool,
}

impl<'a> Node<'a> {
    /// `fields` are the table's; `ids` the fields its relationships use.
    fn new(
        occurrence: &'a SchemaOccurrence,
        fields: &'a [SchemaField],
        ids: &BTreeSet<u64>,
    ) -> Self {
        let all: Vec<&SchemaField> = fields.iter().filter(|f| ids.contains(&f.id)).collect();
        let shown = if all.len() > MAX_KEY_ROWS {
            MAX_KEY_ROWS - 1
        } else {
            all.len()
        };
        let key_rows = all
            .iter()
            .enumerate()
            .map(|(row, field)| (field.id, (row.min(shown), *field)))
            .collect();
        Self {
            occurrence,
            fields,
            hidden_keys: all.len() - shown,
            keys: all.into_iter().take(shown).collect(),
            key_rows,
            graph: occurrence
                .graph
                .unwrap_or_else(|| OccurrenceBox::new_at(0.0, 0.0)),
            // FileMaker Pro writes an empty box for an occurrence it places
            // when it next shows the graph (schema-path.md 5.8, E1): placed
            // here as one without a box.
            stored: occurrence
                .graph
                .is_some_and(|g| g.width > 0.0 && g.height > 0.0),
        }
    }

    fn shows(&self) -> OccurrenceShows {
        self.graph.shows()
    }

    fn key_field(&self, id: u64) -> Option<&'a SchemaField> {
        self.key_rows.get(&id).map(|&(_, field)| field)
    }

    /// Rows in the key-field area, the count row included.
    fn key_row_count(&self) -> usize {
        self.keys.len() + usize::from(self.hidden_keys > 0)
    }

    /// The key-field area with its two edge lines; none without key fields.
    fn key_height(&self) -> f32 {
        match self.key_row_count() {
            0 => 0.0,
            rows => ROW * rows as f32 + 3.0,
        }
    }

    /// The top of the field list's white area.
    fn list_top(&self) -> f32 {
        let keys = self.key_height();
        TITLE + if keys > 0.0 { keys + BAND } else { 0.0 } + 1.0
    }

    fn height(&self) -> f32 {
        match self.shows() {
            OccurrenceShows::NameOnly => TITLE + FRAME,
            OccurrenceShows::KeyFields => TITLE + self.key_height() + FRAME,
            OccurrenceShows::AllFields => {
                // At most about 2,450 points with MAX_KEY_ROWS.
                let least = self.list_top() + ROW + 1.0 + FRAME;
                self.graph.height.clamp(least, LIMIT.max(least))
            }
        }
    }

    /// The box in graph points.
    fn rect(&self) -> Rect {
        Rect::from_min_size(
            pos2(
                self.graph.x.clamp(-LIMIT, LIMIT),
                self.graph.y.clamp(-LIMIT, LIMIT),
            ),
            vec2(self.graph.width.clamp(40.0, 2_000.0), self.height()),
        )
    }

    /// The vertical center of key row `row`, from the box's top.
    fn key_row_y(row: usize) -> f32 {
        TITLE + 1.0 + ROW * row as f32 + ROW / 2.0
    }

    fn field_rects(&self) -> Vec<(Rect, &SchemaField)> {
        let mut rows = Vec::new();
        if self.shows() == OccurrenceShows::NameOnly {
            return rows;
        }
        let bounds = self.rect();
        let row_rect = |y| {
            Rect::from_min_size(
                bounds.min + vec2(SIDE, y),
                vec2(bounds.width() - 2.0 * SIDE, ROW),
            )
        };
        for (row, field) in self.keys.iter().enumerate() {
            rows.push((row_rect(Self::key_row_y(row) - ROW / 2.0), *field));
        }
        if self.shows() == OccurrenceShows::AllFields {
            let top = self.list_top();
            let fit = ((bounds.height() - FRAME - top) / ROW).ceil().max(0.0) as usize;
            let well = Rect::from_min_max(
                bounds.min + vec2(SIDE, top),
                bounds.right_bottom() - vec2(SIDE, FRAME),
            );
            for (row, field) in self.fields.iter().take(fit).enumerate() {
                rows.push((row_rect(top + ROW * row as f32).intersect(well), field));
            }
        }
        rows
    }

    /// Where a relationship on `field` meets the box, from its top: the
    /// key field's row, else the title bar.
    fn anchor_y(&self, field: u64) -> f32 {
        match (self.shows(), self.key_rows.get(&field)) {
            (OccurrenceShows::NameOnly, _) | (_, None) => TITLE / 2.0,
            (_, Some(&(row, _))) => Self::key_row_y(row),
        }
    }
}

/// One relationship as drawn.
struct Link<'a> {
    relationship: &'a SchemaRelationship,
    /// Its left and right occurrences' node indexes.
    nodes: [usize; 2],
    /// Each predicate's left and right field.
    pairs: Vec<[u64; 2]>,
    /// Whether each end is the "one" side.
    one: [bool; 2],
    /// The operator glyph: the shared operator, or each distinct one.
    glyph: String,
    /// Its predicates, one per line: the operator box's tooltip and, joined
    /// by "and", its accessibility label.
    predicates: Vec<String>,
}

/// A link's route: where it meets each box and which way it leaves (+1
/// right, -1 left). Each side's key rows converge on its junction, the end
/// of the first predicate's stub.
struct Route {
    /// Each side's box edge and its distinct key rows.
    edges: [f32; 2],
    rows: [Vec<f32>; 2],
    directions: [f32; 2],
    junctions: [Pos2; 2],
}

struct Graph<'a> {
    nodes: Vec<Node<'a>>,
    links: Vec<Link<'a>>,
}

impl<'a> Graph<'a> {
    fn new(schema: &'a DatabaseSchema) -> Self {
        let index: BTreeMap<u64, usize> = schema
            .occurrences
            .iter()
            .enumerate()
            .map(|(i, occurrence)| (occurrence.id, i))
            .collect();
        let tables: BTreeMap<u64, &'a SchemaTable> = schema
            .tables
            .iter()
            .map(|table| (table.id, table))
            .collect();
        let fields_of = |occurrence: &SchemaOccurrence| -> &'a [SchemaField] {
            occurrence
                .table_id
                .and_then(|id| tables.get(&id))
                .map_or(&[][..], |table| table.fields.as_slice())
        };
        // The relationships that can be drawn, and each side's key fields.
        let mut key_ids = vec![BTreeSet::new(); schema.occurrences.len()];
        let mut drawn = Vec::new();
        for relationship in &schema.relationships {
            let (Some(&left), Some(&right)) = (
                index.get(&relationship.left_occurrence),
                index.get(&relationship.right_occurrence),
            ) else {
                continue;
            };
            if relationship.matches.is_empty() {
                continue;
            }
            for predicate in &relationship.matches {
                key_ids[left].insert(predicate.left_field);
                key_ids[right].insert(predicate.right_field);
            }
            drawn.push((relationship, [left, right]));
        }
        let mut nodes: Vec<Node<'a>> = schema
            .occurrences
            .iter()
            .zip(&key_ids)
            .map(|(occurrence, ids)| Node::new(occurrence, fields_of(occurrence), ids))
            .collect();
        let links = drawn
            .into_iter()
            .map(|(relationship, ends)| {
                let field = |side: usize, id: u64| nodes[ends[side]].key_field(id);
                // An `=` on a unique field matches at most one record [I].
                let one = |side: usize| {
                    relationship.matches.iter().any(|predicate| {
                        let id = [predicate.left_field, predicate.right_field][side];
                        predicate.comparison == "=" && field(side, id).is_some_and(|f| f.unique)
                    })
                };
                let mut glyph = String::new();
                for predicate in &relationship.matches {
                    if !glyph.contains(predicate.comparison.as_str()) {
                        glyph.push_str(&predicate.comparison);
                    }
                }
                let name = |side: usize, id: u64| field(side, id).map_or("?", |f| f.name.as_str());
                let predicates = relationship
                    .matches
                    .iter()
                    .map(|predicate| {
                        format!(
                            "{}::{} {} {}::{}",
                            nodes[ends[0]].occurrence.name,
                            name(0, predicate.left_field),
                            predicate.comparison,
                            nodes[ends[1]].occurrence.name,
                            name(1, predicate.right_field),
                        )
                    })
                    .collect();
                Link {
                    relationship,
                    nodes: ends,
                    pairs: relationship
                        .matches
                        .iter()
                        .map(|predicate| [predicate.left_field, predicate.right_field])
                        .collect(),
                    one: [one(0), one(1)],
                    glyph,
                    predicates,
                }
            })
            .collect::<Vec<_>>();
        place_unstored(&mut nodes, &links);
        Self { nodes, links }
    }

    /// The graph's size in points, and the offset that brings boxes stored
    /// at negative coordinates into view.
    fn extent(&self) -> (Vec2, Vec2) {
        let bounds = self
            .nodes
            .iter()
            .map(Node::rect)
            .reduce(|a, b| a.union(b))
            .unwrap_or(Rect::ZERO);
        let shift = vec2((-bounds.left()).max(0.0), (-bounds.top()).max(0.0));
        (bounds.max.to_vec2() + shift + Vec2::splat(MARGIN), shift)
    }

    fn route(&self, link: &Link<'_>) -> Route {
        let [a, b] = link.nodes;
        let (ra, rb) = (self.nodes[a].rect(), self.nodes[b].rect());
        let rows = |side: usize, rect: Rect| {
            let node = &self.nodes[link.nodes[side]];
            let mut ys: Vec<f32> = link
                .pairs
                .iter()
                .map(|pair| rect.top() + node.anchor_y(pair[side]))
                .collect();
            // The first predicate's row leads; the rest follow in order.
            let first = ys[0];
            ys.sort_by(f32::total_cmp);
            ys.dedup();
            ys.retain(|&y| y != first);
            ys.insert(0, first);
            ys
        };
        let rows = [rows(0, ra), rows(1, rb)];
        // Facing sides when the boxes are apart; boxes in one column are
        // joined on their right [OpenMaker's choice, not compared].
        let (da, db) = if rb.left() > ra.right() {
            (1.0, -1.0)
        } else if ra.left() > rb.right() {
            (-1.0, 1.0)
        } else {
            (1.0, 1.0)
        };
        let edge = |rect: Rect, direction: f32| {
            if direction > 0.0 {
                rect.right()
            } else {
                rect.left()
            }
        };
        let edges = [edge(ra, da), edge(rb, db)];
        let junctions = if da == db {
            let x = ra.right().max(rb.right()) + STUB;
            [pos2(x, rows[0][0]), pos2(x, rows[1][0])]
        } else {
            [
                pos2(edges[0] + da * STUB, rows[0][0]),
                pos2(edges[1] + db * STUB, rows[1][0]),
            ]
        };
        Route {
            edges,
            rows,
            directions: [da, db],
            junctions,
        }
    }

    fn paint(
        &self,
        ui: &mut Ui,
        schema: &DatabaseSchema,
        selection: &Selection,
        editable: bool,
        view: &mut View,
        events: &mut Vec<GraphEvent>,
    ) {
        let zoom = view.factor();
        let (extent, shift) = self.extent();
        let size = (extent * zoom).max(ui.available_size());
        let (canvas, _) = ui.allocate_exact_size(size, Sense::hover());
        let scale = Scale {
            origin: canvas.min + shift * zoom,
            zoom,
        };
        // One-pixel lines centred on pixel centres, not across two rows.
        let lines = Scale {
            origin: scale.origin + Vec2::splat(0.5),
            zoom,
        };
        let painter = ui.painter().clone();
        let visible = painter.clip_rect();
        let routes: Vec<Route> = self.links.iter().map(|link| self.route(link)).collect();
        // Which sides of each key row a line leaves by: [left, right].
        let mut markers: BTreeMap<(usize, u64), [bool; 2]> = BTreeMap::new();
        for (link, route) in self.links.iter().zip(&routes) {
            for pair in &link.pairs {
                for ((node, field), direction) in link.nodes.iter().zip(pair).zip(route.directions)
                {
                    markers.entry((*node, *field)).or_default()[usize::from(direction > 0.0)] =
                        true;
                }
            }
        }
        let line = Stroke::new(zoom.max(1.0), Color32::BLACK);
        let polyline = |points: &[Pos2]| {
            painter.add(Shape::line(
                points.iter().map(|&p| lines.pos(p)).collect(),
                line,
            ));
        };
        for (link, route) in self.links.iter().zip(&routes) {
            polyline(&[route.junctions[0], route.junctions[1]]);
            for side in 0..2 {
                let (edge, direction) = (route.edges[side], route.directions[side]);
                for &y in &route.rows[side] {
                    // A stub from each key row; a compound relationship's
                    // rows converge on the first one's junction.
                    let anchor = pos2(edge, y);
                    polyline(&[
                        anchor,
                        anchor + vec2(direction * STUB / 2.0, 0.0),
                        route.junctions[side],
                    ]);
                    if !link.one[side] {
                        let apex = anchor + vec2(direction * FOOT, 0.0);
                        for spread in [-SPREAD, SPREAD] {
                            polyline(&[apex, anchor + vec2(0.0, spread)]);
                        }
                    }
                }
            }
            let center = route.junctions[0] + (route.junctions[1] - route.junctions[0]) / 2.0;
            let on = selection.relationship == Some(link.relationship.id);
            let rect = paint_operator(&painter, &scale, center, &link.glyph);
            if on {
                painter.rect_stroke(
                    rect.expand(2.0),
                    CornerRadius::same(2),
                    Stroke::new(2.0, theme::SELECTED),
                    StrokeKind::Outside,
                );
            }
            let response = ui.interact(
                rect.expand(3.0),
                ui.scope_id().with(("relationship", link.relationship.id)),
                Sense::click(),
            );
            response.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, on, link.predicates.join(" and "))
            });
            // Every predicate on hover [OpenMaker's choice: FileMaker's
            // tooltip is not captured].
            let response = response.on_hover_text(link.predicates.join("\n"));
            if response.double_clicked() {
                events.push(GraphEvent::EditRelationship(link.relationship.id));
            } else if response.clicked() {
                events.push(GraphEvent::SelectRelationship(link.relationship.id));
            }
        }
        // A drop lands in the topmost box under the pointer, the last drawn:
        // rows of a box beneath it share this layer and contain the pointer
        // too.
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let top = pointer.and_then(|p| {
            self.nodes
                .iter()
                .rposition(|n| scale.rect(n.rect()).intersect(visible).contains(p))
        });
        for (index, node) in self.nodes.iter().enumerate() {
            let rect = scale.rect(node.rect());
            // Only what is in view is painted; every box stays reachable
            // to accessibility.
            if rect.intersects(visible) {
                paint_node(
                    &painter,
                    &scale,
                    node,
                    selection.has(node.occurrence.id),
                    |field| markers.get(&(index, field)).copied().unwrap_or_default(),
                );
            }
            let response = ui.interact(
                rect,
                ui.scope_id().with(("occurrence", node.occurrence.id)),
                // A box OpenMaker placed (an empty stored box) is saved
                // where it is dropped, at the size it is drawn.
                if editable {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            response.widget_info(|| {
                WidgetInfo::selected(
                    Role::ListBoxOption,
                    true,
                    selection.has(node.occurrence.id),
                    &node.occurrence.name,
                )
            });
            let id = node.occurrence.id;
            if response.double_clicked() {
                events.push(GraphEvent::EditOccurrence(id));
            } else if response.clicked() && shift_click(ui) {
                events.push(GraphEvent::ToggleOccurrence(id));
            } else if response.clicked() || response.drag_started() {
                events.push(GraphEvent::SelectOccurrence(id));
            }
            if response.dragged() {
                let offset = view
                    .drag
                    .filter(|(d, _)| *d == id)
                    .map_or(Vec2::ZERO, |(_, o)| o);
                view.drag = Some((id, offset + response.drag_delta() / zoom));
            }
            if response.drag_stopped() {
                if let Some((_, offset)) = view.drag.take().filter(|(d, _)| *d == id) {
                    // The box already holds the offset while it is drawn.
                    let moved = node.rect();
                    if offset != Vec2::ZERO {
                        events.push(GraphEvent::Arrange {
                            occurrence: id,
                            left: moved.left().round() as i32,
                            top: moved.top().round() as i32,
                            width: node.graph.width.round() as i32,
                            height: node.graph.height.round() as i32,
                        });
                    }
                }
            }
            // Rows register after the box, so dragging a field links it;
            // dragging the title/background still moves the occurrence.
            if rect.intersects(visible) {
                for (slot, (row, field)) in node.field_rects().into_iter().enumerate() {
                    let row = scale.rect(row).intersect(visible);
                    if !row.is_positive() {
                        continue;
                    }
                    let matchable = field.data_type != FieldKind::Container
                        && field.kind != SchemaFieldKind::Summary
                        && !field.unstored;
                    // As Edit Relationship: what the graph may change.
                    let enabled = editable && ui.is_enabled() && matchable;
                    let response = ui.interact(
                        row,
                        ui.scope_id().with(("match_field", id, field.id, slot)),
                        if enabled {
                            Sense::click_and_drag()
                        } else {
                            Sense::hover()
                        },
                    );
                    response.widget_info(|| {
                        WidgetInfo::labeled(
                            Role::Button,
                            enabled,
                            format!("Match field {}::{}", node.occurrence.name, field.name),
                        )
                    });
                    if response.clicked() {
                        events.push(match shift_click(ui) {
                            true => GraphEvent::ToggleOccurrence(id),
                            false => GraphEvent::SelectOccurrence(id),
                        });
                    }
                    if response.double_clicked() {
                        events.push(GraphEvent::EditOccurrence(id));
                    }
                    if enabled && response.drag_started() {
                        response.dnd_set_drag_payload(FieldDrag {
                            from: (id, field.id),
                            origin: response.rect.center(),
                            schema: std::sync::Arc::new(schema.clone()),
                        });
                    }
                    // Rows share their edges: one on the line between two
                    // belongs to the lower row, as egui's own hit test has it.
                    let over = pointer.is_some_and(|p| p.y < row.max.y);
                    if enabled && top == Some(index) && over {
                        if let Some(source) = response.dnd_release_payload::<FieldDrag>() {
                            if source.from == (id, field.id) {
                                // Released where it started: no link.
                            } else if source.schema.as_ref() == schema {
                                events.push(GraphEvent::LinkFields {
                                    from: source.from,
                                    to: (id, field.id),
                                });
                            } else {
                                events.push(GraphEvent::StaleFieldLink);
                            }
                        }
                    }
                    if !matchable {
                        response.on_hover_text(
                            "OpenMaker cannot use this field as a graph match field yet.",
                        );
                    }
                }
            }
        }
        if let Some(source) = egui::DragAndDrop::payload::<FieldDrag>(ui.ctx()) {
            if source.schema.as_ref() == schema {
                if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                    painter
                        .line_segment([source.origin, pointer], Stroke::new(1.0, theme::SELECTED));
                }
            }
        }
    }
}

/// Place occurrences whose definition holds no readable box, below any
/// stored ones: each connected group in columns by its distance from its
/// first occurrence, 131 x 118 boxes 20 points apart [OpenMaker's choice].
fn place_unstored(nodes: &mut [Node<'_>], links: &[Link<'_>]) {
    if nodes.iter().all(|node| node.stored) {
        return;
    }
    let mut neighbours = vec![Vec::new(); nodes.len()];
    for link in links {
        let [a, b] = link.nodes;
        neighbours[a].push(b);
        neighbours[b].push(a);
    }
    let mut top = nodes
        .iter()
        .filter(|node| node.stored)
        .map(|node| node.rect().bottom() + 40.0)
        .fold(20.0_f32, f32::max);
    let mut column_of: Vec<Option<usize>> = vec![None; nodes.len()];
    for start in 0..nodes.len() {
        if nodes[start].stored || column_of[start].is_some() {
            continue;
        }
        // Breadth first through the unplaced occurrences.
        let mut group = Vec::new();
        let mut queue = VecDeque::from([(start, 0)]);
        column_of[start] = Some(0);
        while let Some((node, column)) = queue.pop_front() {
            group.push((node, column));
            for &next in &neighbours[node] {
                if !nodes[next].stored && column_of[next].is_none() {
                    column_of[next] = Some(column + 1);
                    queue.push_back((next, column + 1));
                }
            }
        }
        let mut bottoms: Vec<f32> = Vec::new();
        for (node, column) in group {
            if bottoms.len() <= column {
                bottoms.resize(column + 1, top);
            }
            let y = bottoms[column];
            // A stored but empty box keeps its colour.
            let color = nodes[node].occurrence.graph.map(|g| g.color);
            nodes[node].graph = OccurrenceBox::new_at(20.0 + 190.0 * column as f32, y);
            if let Some(color) = color {
                nodes[node].graph.color = color;
            }
            bottoms[column] = y + nodes[node].height() + 20.0;
        }
        top = bottoms.into_iter().fold(top, f32::max) + 20.0;
    }
}

/// Graph points to screen points.
struct Scale {
    origin: Pos2,
    zoom: f32,
}

impl Scale {
    fn pos(&self, p: Pos2) -> Pos2 {
        self.origin + p.to_vec2() * self.zoom
    }

    fn rect(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.pos(r.min), self.pos(r.max))
    }

    fn len(&self, length: f32) -> f32 {
        length * self.zoom
    }

    fn font(&self) -> FontId {
        FontId::proportional(FONT * self.zoom)
    }
}

/// The shades of one occurrence colour. FileMaker draws the default grey
/// `78 78 78` as these tints, measured; other colours are assumed to tint
/// alike.
struct Palette {
    face: Color32,
    highlight: Color32,
    light: Color32,
    shade: Color32,
    dark: Color32,
    outline: Color32,
    band_top: Color32,
    band_bottom: Color32,
    shadow: Color32,
    button_shadow: Color32,
}

impl Palette {
    fn new([r, g, b, _]: [u8; 4]) -> Self {
        let tint = |t: f32| {
            let channel = |c: u8| (f32::from(c) + (255.0 - f32::from(c)) * t).round() as u8;
            Color32::from_rgb(channel(r), channel(g), channel(b))
        };
        let shade = |t: f32| {
            let channel = |c: u8| (f32::from(c) * t).round() as u8;
            Color32::from_rgb(channel(r), channel(g), channel(b))
        };
        Self {
            face: tint(0.55),
            highlight: tint(0.87),
            light: tint(0.66),
            shade: tint(0.39),
            dark: tint(0.22),
            outline: tint(0.33),
            band_top: tint(0.73),
            band_bottom: tint(0.45),
            shadow: shade(0.9),
            button_shadow: shade(1.02),
        }
    }
}

fn paint_node(
    painter: &Painter,
    scale: &Scale,
    node: &Node<'_>,
    selected: bool,
    markers: impl Fn(u64) -> [bool; 2],
) {
    let palette = Palette::new(node.graph.color);
    let bounds = node.rect();
    let rect = scale.rect(bounds);
    let radius = scale.len(RADIUS);
    let corner = CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
    let at = |x: f32, y: f32| scale.pos(bounds.min + vec2(x, y));
    let one = scale.len(1.0);
    painter.rect_filled(rect, corner, palette.face);
    band(
        painter,
        rect,
        radius,
        [at(0.0, 1.0).y, at(0.0, 6.0).y],
        [palette.highlight, palette.face],
    );
    band(
        painter,
        rect,
        radius,
        [at(0.0, bounds.height() - FRAME).y, rect.bottom()],
        [palette.face, palette.shadow],
    );
    // Raised sides: a dark outer edge, lit inside on the left and shaded
    // inside on the right.
    let side = |x: f32, color: Color32| {
        painter.rect_filled(
            Rect::from_min_max(at(x, RADIUS), at(x + 1.0, bounds.height() - RADIUS)),
            CornerRadius::ZERO,
            color,
        );
    };
    let width = bounds.width();
    side(0.0, palette.outline);
    side(1.0, palette.light);
    side(width - 2.0, palette.shade);
    side(width - 1.0, palette.dark);
    let inner = |top: f32, bottom: f32| Rect::from_min_max(at(SIDE, top), at(width - SIDE, bottom));
    // The title bar: the go-to-table arrow, the name, the display button.
    painter.line_segment(
        [at(4.5, 12.5), at(7.5, 9.0)],
        Stroke::new(scale.len(1.3), Color32::BLACK),
    );
    painter.add(Shape::convex_polygon(
        vec![at(5.0, 8.0), at(8.5, 8.0), at(8.5, 11.5)],
        Color32::BLACK,
        Stroke::NONE,
    ));
    put_text(
        &painter.with_clip_rect(Rect::from_min_max(at(10.5, 0.0), at(width - 14.0, TITLE))),
        scale,
        at(10.5, TITLE / 2.0 + 1.0),
        &node.occurrence.name,
        Color32::BLACK,
        false,
    );
    // A raised white button whose glyph shows the display.
    let button = Rect::from_min_size(at(width - 13.0, 5.0), Vec2::splat(scale.len(8.0)));
    painter.rect_filled(
        button.translate(Vec2::splat(one)),
        CornerRadius::ZERO,
        palette.button_shadow,
    );
    painter.rect_filled(button, CornerRadius::ZERO, Color32::WHITE);
    let glyph = Rect::from_min_size(
        button.min + Vec2::splat(one),
        vec2(scale.len(6.0), scale.len(5.0)),
    );
    let black = Stroke::new(one, Color32::BLACK);
    match node.shows() {
        OccurrenceShows::AllFields => {
            painter.rect_stroke(glyph, CornerRadius::ZERO, black, StrokeKind::Inside);
            painter.line_segment(
                [
                    pos2(glyph.left(), glyph.center().y),
                    pos2(glyph.right(), glyph.center().y),
                ],
                black,
            );
        }
        OccurrenceShows::KeyFields => {
            painter.rect_stroke(glyph, CornerRadius::ZERO, black, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_size(glyph.min, vec2(glyph.width(), scale.len(2.0))),
                CornerRadius::ZERO,
                Color32::BLACK,
            );
        }
        // Inferred, not observed.
        OccurrenceShows::NameOnly => {
            painter.rect_filled(
                Rect::from_min_size(glyph.min, vec2(glyph.width(), scale.len(2.0))),
                CornerRadius::ZERO,
                Color32::BLACK,
            );
        }
    }
    if node.shows() == OccurrenceShows::NameOnly {
        return selection(painter, rect, corner, selected);
    }
    // The key fields, with a marker on each side a relationship leaves by;
    // only the rows in view are painted.
    let key_height = node.key_height();
    let grey = Color32::from_gray(0x99);
    if key_height > 0.0 {
        let well = inner(TITLE, TITLE + key_height);
        sunken(painter, scale, well, &palette);
        let clip = painter.with_clip_rect(well.shrink(one));
        let first = at(0.0, Node::key_row_y(0) - ROW / 2.0).y;
        for row in visible_rows(
            first,
            scale.len(ROW),
            node.key_row_count(),
            clip.clip_rect(),
        ) {
            let y = Node::key_row_y(row);
            let Some(field) = node.keys.get(row) else {
                let more = format!("{} more key fields", node.hidden_keys);
                put_text(&clip, scale, at(TEXT_X, y), &more, grey, false);
                continue;
            };
            put_text(
                &clip,
                scale,
                at(TEXT_X, y),
                &field.name,
                Color32::BLACK,
                false,
            );
            let [left, right] = markers(field.id);
            for (on, x) in [(left, MARKER_X), (right, width - MARKER_X)] {
                if on {
                    clip.circle_stroke(at(x, y), one, Stroke::new(one, Color32::BLACK));
                }
            }
        }
    }
    // Every field: key fields in italic, fields that cannot be match fields
    // in grey.
    if node.shows() == OccurrenceShows::AllFields {
        let list_top = node.list_top();
        if key_height > 0.0 {
            let top = TITLE + key_height;
            band(
                painter,
                inner(top, top + BAND),
                0.0,
                [at(0.0, top).y, at(0.0, top + BAND).y],
                [palette.band_top, palette.band_bottom],
            );
        }
        let well = inner(list_top - 1.0, bounds.height() - FRAME);
        sunken(painter, scale, well, &palette);
        let clip = painter.with_clip_rect(well.shrink(one));
        let fit = ((bounds.height() - FRAME - list_top) / ROW).ceil().max(0.0) as usize;
        let rows = visible_rows(
            at(0.0, list_top).y,
            scale.len(ROW),
            node.fields.len().min(fit),
            clip.clip_rect(),
        );
        for row in rows {
            let field = &node.fields[row];
            let matchable = field.data_type != FieldKind::Container
                && field.kind != SchemaFieldKind::Summary
                && !field.unstored;
            let color = if matchable { Color32::BLACK } else { grey };
            let key = node.key_rows.contains_key(&field.id);
            let y = list_top + ROW * row as f32 + ROW / 2.0;
            put_text(&clip, scale, at(TEXT_X, y), &field.name, color, key);
        }
    }
    selection(painter, rect, corner, selected);
}

/// The rows of `count`, each `pitch` tall from `top`, that meet `clip`.
fn visible_rows(top: f32, pitch: f32, count: usize, clip: Rect) -> Range<usize> {
    if pitch <= 0.0 || clip.bottom() <= top {
        return 0..0;
    }
    let first = ((clip.top() - top) / pitch).floor().max(0.0) as usize;
    let last = ((clip.bottom() - top) / pitch).ceil().max(0.0) as usize;
    first.min(count)..last.min(count)
}

/// Graph text in [`FONT`] with [`LETTER_SPACING`], its left edge's middle
/// at `left_center`.
fn put_text(
    painter: &Painter,
    scale: &Scale,
    left_center: Pos2,
    text: &str,
    color: Color32,
    italics: bool,
) {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), scale.font(), color);
    for section in &mut job.sections {
        section.format.extra_letter_spacing = LETTER_SPACING * scale.zoom;
        section.format.italics = italics;
    }
    let galley = painter.layout_job(job);
    painter.galley(
        left_center - vec2(0.0, galley.size().y / 2.0),
        galley,
        color,
    );
}

/// The selected occurrence's outline [OpenMaker's choice: FileMaker's is
/// not captured].
fn selection(painter: &Painter, rect: Rect, corner: CornerRadius, selected: bool) {
    if selected {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(2.0, theme::SELECTED),
            StrokeKind::Outside,
        );
    }
}

/// A white area set into the box: a dark top and left edge, a light bottom
/// and right edge.
fn sunken(painter: &Painter, scale: &Scale, rect: Rect, palette: &Palette) {
    let edge = scale.len(1.0);
    painter.rect_filled(rect, CornerRadius::ZERO, palette.light);
    painter.rect_filled(
        Rect::from_min_max(rect.min, rect.max - Vec2::splat(edge)),
        CornerRadius::ZERO,
        palette.dark,
    );
    painter.rect_filled(rect.shrink(edge), CornerRadius::ZERO, Color32::WHITE);
}

/// A vertical gradient from `colors[0]` at `ys[0]` to `colors[1]` at
/// `ys[1]`, across `outer` less its rounded corners of `radius`.
fn band(painter: &Painter, outer: Rect, radius: f32, ys: [f32; 2], colors: [Color32; 2]) {
    const STEPS: u32 = 6;
    let mut mesh = Mesh::default();
    for step in 0..=STEPS {
        let t = step as f32 / STEPS as f32;
        let y = ys[0] + (ys[1] - ys[0]) * t;
        let edge = (y - outer.top()).min(outer.bottom() - y).max(0.0);
        let inset = if edge < radius {
            radius - (radius * radius - (radius - edge).powi(2)).max(0.0).sqrt()
        } else {
            0.0
        };
        let color = colors[0].lerp_to_gamma(colors[1], t);
        mesh.colored_vertex(pos2(outer.left() + inset, y), color);
        mesh.colored_vertex(pos2(outer.right() - inset, y), color);
        if step > 0 {
            let base = 2 * step;
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// The framed operator centered on a relationship line; returns its rect.
fn paint_operator(painter: &Painter, scale: &Scale, center: Pos2, glyph: &str) -> Rect {
    let font = FontId::proportional(10.0 * scale.zoom);
    let galley = painter.layout_no_wrap(glyph.to_owned(), font, Color32::BLACK);
    let width = OPERATOR.x.max(galley.size().x / scale.zoom.max(0.01) + 6.0);
    let size = vec2(width, OPERATOR.y) * scale.zoom;
    let rect = Rect::from_min_size((scale.pos(center) - size / 2.0).round(), size.round());
    painter.rect(
        rect,
        CornerRadius::ZERO,
        Color32::WHITE,
        Stroke::new(scale.len(1.0).max(1.0), Color32::BLACK),
        StrokeKind::Inside,
    );
    if glyph == "=" {
        // FileMaker draws `=` as two wide bars.
        for dy in [-1.5, 1.5] {
            let y = rect.center().y + scale.len(dy);
            painter.line_segment(
                [
                    pos2(rect.center().x - scale.len(4.0), y),
                    pos2(rect.center().x + scale.len(4.0), y),
                ],
                Stroke::new(scale.len(1.0).max(1.0), Color32::BLACK),
            );
        }
    } else {
        painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::BLACK);
    }
    rect
}

/// The glyphs of the tools under the graph, drawn on a 16-point grid.
#[derive(Clone, Copy)]
enum Glyph {
    AddOccurrence,
    AddRelationship,
    Duplicate,
    Edit,
    Delete,
    Color,
    AlignLeft,
    AlignTop,
    Distribute,
    Resize,
    Select,
    TextNote,
    TableDisplay,
    SelectTables,
    ZoomIn,
    ZoomOut,
    PageBreaks,
    PageSetup,
}

/// Whether this frame's click came with Shift: held now, or given with the
/// button's release (a press and release can arrive in one frame).
fn shift_click(ui: &Ui) -> bool {
    ui.input(|i| {
        i.modifiers.shift
            || i.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers,
                        ..
                    } if modifiers.shift
                )
            })
    })
}

/// FileMaker's tool groups under the graph. Only selection and zoom work;
/// the rest edit the graph and are shown unavailable.
fn tools(
    ui: &mut Ui,
    view: &mut View,
    editable: bool,
    selection: &Selection,
    events: &mut Vec<GraphEvent>,
) {
    let any = selection.occurrence.is_some() || selection.relationship.is_some();
    // Edit, Delete and colour act on one object.
    let one = any && selection.also.is_empty();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        group(ui, "Tables / Relationships", |ui| {
            for (glyph, label, enabled, event) in [
                (
                    Glyph::AddOccurrence,
                    "Add a table occurrence",
                    editable,
                    Some(GraphEvent::AddOccurrence),
                ),
                (
                    Glyph::AddRelationship,
                    "Add a relationship",
                    editable,
                    Some(GraphEvent::AddRelationship),
                ),
                (
                    Glyph::Duplicate,
                    "Duplicate selected objects",
                    editable && selection.occurrence.is_some(),
                    Some(GraphEvent::Duplicate),
                ),
                (
                    Glyph::Edit,
                    "Edit the selected object",
                    editable && one,
                    Some(GraphEvent::EditSelected),
                ),
                (
                    Glyph::Delete,
                    "Delete selected objects",
                    editable && one,
                    Some(GraphEvent::DeleteSelected),
                ),
            ] {
                let response = tool(ui, glyph, label, enabled, false, editable);
                if response.clicked() {
                    events.extend(event);
                }
            }
            let colour = tool(
                ui,
                Glyph::Color,
                "Table occurrence color",
                editable && one && selection.occurrence.is_some(),
                false,
                editable,
            );
            egui::Popup::menu(&colour).show(|ui| {
                ui.horizontal(|ui| {
                    for color in COLORS {
                        let (rect, swatch) =
                            ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                        ui.painter().rect(
                            rect,
                            CornerRadius::same(3),
                            Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
                            Stroke::new(1.0, Color32::from_gray(0x60)),
                            StrokeKind::Inside,
                        );
                        let name = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
                        swatch.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
                        if swatch.clicked() {
                            events.push(GraphEvent::Color(color));
                            ui.close();
                        }
                    }
                });
            });
        });
        group(ui, "Arrange", |ui| {
            use om_data::Arrangement::*;
            let resize = |largest, width, height| Resize {
                largest,
                width,
                height,
            };
            // Claris Help's options for each tool.
            let menus = [
                (
                    Glyph::AlignLeft,
                    "Align objects",
                    vec![
                        ("Align Left Edges", AlignLeft),
                        ("Align Centers", AlignCenter),
                        ("Align Right Edges", AlignRight),
                    ],
                ),
                (
                    Glyph::AlignTop,
                    "Align objects vertically",
                    vec![
                        ("Align Top Edges", AlignTop),
                        ("Align Centers", AlignMiddle),
                        ("Align Bottom Edges", AlignBottom),
                    ],
                ),
                (
                    Glyph::Distribute,
                    "Distribute objects",
                    vec![
                        ("Distribute Horizontally", DistributeHorizontally),
                        ("Distribute Vertically", DistributeVertically),
                    ],
                ),
                (
                    Glyph::Resize,
                    "Resize objects",
                    vec![
                        ("Smallest Width", resize(false, true, false)),
                        ("Smallest Height", resize(false, false, true)),
                        ("Smallest Width and Height", resize(false, true, true)),
                        ("Largest Width", resize(true, true, false)),
                        ("Largest Height", resize(true, false, true)),
                        ("Largest Width and Height", resize(true, true, true)),
                    ],
                ),
            ];
            // They arrange two or more boxes.
            let several = editable && selection.occurrence.is_some() && !selection.also.is_empty();
            for (glyph, label, options) in menus {
                let response = tool(ui, glyph, label, several, false, editable);
                egui::Popup::menu(&response).show(|ui| {
                    for (option, how) in options {
                        if ui.button(option).clicked() {
                            events.push(GraphEvent::ArrangeSelected(how));
                            ui.close();
                        }
                    }
                });
            }
        });
        group(ui, "Tools", |ui| {
            tool(ui, Glyph::Select, "Selection tool", true, true, true);
            tool(ui, Glyph::TextNote, "Text note tool", false, false, false);
            tool(
                ui,
                Glyph::TableDisplay,
                "Table display",
                false,
                false,
                false,
            );
            tool(
                ui,
                Glyph::SelectTables,
                "Select tables",
                false,
                false,
                false,
            );
            if tool(ui, Glyph::ZoomIn, "Zoom in", true, false, true).clicked() {
                view.zoom_in();
            }
            if tool(ui, Glyph::ZoomOut, "Zoom out", true, false, true).clicked() {
                view.zoom_out();
            }
            ui.add_space(4.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut view.text)
                    .desired_width(30.0)
                    .margin(egui::Margin::symmetric(4, 2))
                    .align(Align2::CENTER_CENTER)
                    .char_limit(3),
            );
            widgets::accessible_label(ui, response.id, "Zoom percentage");
            if response.lost_focus() {
                match view.text.trim().parse::<u16>() {
                    Ok(percent) => view.set(percent),
                    Err(_) => view.text = view.percent.to_string(),
                }
            }
            ui.label("%");
        });
        group(ui, "Pages", |ui| {
            tool(
                ui,
                Glyph::PageBreaks,
                "Show page breaks",
                false,
                false,
                false,
            );
            tool(
                ui,
                Glyph::PageSetup,
                "Graph page setup",
                false,
                false,
                false,
            );
        });
    });
}

/// A captioned group of tools on a rounded panel.
fn group(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        // Painted, so the caption cannot be mistaken for a control of the
        // same name (the Tools menu).
        let font = theme::body();
        let width = widgets::text_width(ui, caption, font.clone());
        let (rect, _) = ui.allocate_exact_size(vec2(width + 12.0, 16.0), Sense::hover());
        ui.painter().text(
            rect.left_center() + vec2(12.0, 0.0),
            Align2::LEFT_CENTER,
            caption,
            font,
            theme::TEXT,
        );
        egui::Frame::new()
            .fill(Color32::from_gray(0xE9))
            .stroke(Stroke::new(1.0, Color32::from_gray(0xDB)))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(10, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    add(ui);
                });
            });
    });
}

/// One tool button. `active` is the current tool; `built` tells a tool
/// OpenMaker lacks from one that only waits for a selection or a schema
/// that can be saved.
fn tool(
    ui: &mut Ui,
    glyph: Glyph,
    label: &str,
    enabled: bool,
    active: bool,
    built: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(23.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter_at(rect);
    if active {
        painter.rect_filled(rect, CornerRadius::same(3), Color32::from_gray(0xD6));
    }
    let color = if !enabled {
        Color32::from_gray(0xAF)
    } else if active {
        Color32::from_rgb(0x1F, 0x4F, 0xF0)
    } else if response.is_pointer_button_down_on() {
        Color32::from_gray(0x45)
    } else {
        Color32::from_gray(0x74)
    };
    paint_glyph(&painter, rect.shrink(2.5), glyph, color);
    response.widget_info(|| {
        if active {
            WidgetInfo::selected(Role::Button, enabled, true, label)
        } else {
            WidgetInfo::labeled(Role::Button, enabled, label)
        }
    });
    if enabled {
        response.on_hover_text(label)
    } else if built {
        response.on_hover_text(format!("{label}: {READ_ONLY}"))
    } else {
        response.on_hover_text(format!("{label}: {NOT_BUILT}"))
    }
}

fn paint_glyph(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
    let unit = rect.width().min(rect.height()) / 16.0;
    let origin = rect.center() - Vec2::splat(8.0 * unit);
    let p = |x: f32, y: f32| origin + vec2(x, y) * unit;
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_two_pos(p(x0, y0), p(x1, y1));
    let stroke = Stroke::new((1.2 * unit).max(1.0), color);
    let line = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    let outline = |rect: Rect, radius: u8| {
        painter.rect_stroke(rect, CornerRadius::same(radius), stroke, StrokeKind::Middle);
    };
    let fill = |rect: Rect| painter.rect_filled(rect, CornerRadius::ZERO, color);
    // The small menu corner FileMaker puts on the Arrange tools.
    let corner = || {
        painter.add(Shape::convex_polygon(
            vec![p(16.0, 13.5), p(16.0, 16.0), p(13.5, 16.0)],
            color,
            Stroke::NONE,
        ));
    };
    let magnifier = |plus: bool| {
        painter.circle_stroke(p(6.5, 6.5), 4.8 * unit, stroke);
        line(&[(10.0, 10.0), (14.5, 14.5)]);
        line(&[(4.3, 6.5), (8.7, 6.5)]);
        if plus {
            line(&[(6.5, 4.3), (6.5, 8.7)]);
        }
    };
    match glyph {
        Glyph::AddOccurrence => {
            outline(r(1.5, 2.0, 13.5, 13.0), 1);
            fill(r(1.5, 2.0, 13.5, 4.5));
            painter.circle_filled(p(12.5, 12.5), 3.6 * unit, Color32::from_gray(0xE9));
            line(&[(12.5, 9.5), (12.5, 15.5)]);
            line(&[(9.5, 12.5), (15.5, 12.5)]);
        }
        Glyph::AddRelationship => {
            outline(r(4.0, 4.5, 12.0, 11.5), 2);
            line(&[(6.2, 7.0), (9.8, 7.0)]);
            line(&[(6.2, 9.0), (9.8, 9.0)]);
            line(&[(0.5, 8.0), (4.0, 8.0)]);
            line(&[(12.0, 8.0), (15.5, 8.0)]);
        }
        Glyph::Duplicate => {
            line(&[(1.5, 10.5), (1.5, 1.5), (10.5, 1.5)]);
            outline(r(4.5, 4.5, 14.5, 14.5), 2);
            line(&[(9.5, 7.0), (9.5, 12.0)]);
            line(&[(7.0, 9.5), (12.0, 9.5)]);
        }
        Glyph::Edit => icons::paint(painter, rect, Icon::Pencil, color, color),
        Glyph::Delete => {
            line(&[(1.5, 3.5), (14.5, 3.5)]);
            line(&[(5.5, 3.5), (5.5, 1.5), (10.5, 1.5), (10.5, 3.5)]);
            line(&[(3.0, 3.5), (4.0, 15.0), (12.0, 15.0), (13.0, 3.5)]);
            line(&[(6.5, 6.0), (6.5, 12.5)]);
            line(&[(9.5, 6.0), (9.5, 12.5)]);
        }
        Glyph::Color => {
            painter.rect_stroke(
                r(0.5, 0.5, 15.5, 15.5),
                CornerRadius::ZERO,
                Stroke::new(unit.max(1.0), color),
                StrokeKind::Inside,
            );
            painter.rect_filled(r(2.0, 2.0, 14.0, 14.0), CornerRadius::ZERO, color);
        }
        Glyph::AlignLeft => {
            line(&[(1.5, 0.5), (1.5, 15.5)]);
            outline(r(3.5, 3.0, 9.5, 6.5), 0);
            outline(r(3.5, 9.0, 12.5, 12.5), 0);
            corner();
        }
        Glyph::AlignTop => {
            line(&[(0.5, 1.5), (13.5, 1.5)]);
            outline(r(3.0, 3.5, 6.5, 9.5), 0);
            outline(r(8.5, 3.5, 12.0, 13.0), 0);
            corner();
        }
        Glyph::Distribute => {
            line(&[(0.5, 8.0), (14.5, 8.0)]);
            for x in [2.0, 13.0] {
                line(&[(x, 2.0), (x, 14.0)]);
            }
            outline(r(4.5, 4.0, 7.0, 12.0), 0);
            outline(r(8.5, 4.0, 11.0, 12.0), 0);
            corner();
        }
        Glyph::Resize => {
            line(&[(1.5, 1.5), (1.5, 14.5)]);
            for x in [4.0, 7.0, 10.0] {
                line(&[(x, 1.5), (x + 1.5, 1.5)]);
                line(&[(x, 14.5), (x + 1.5, 14.5)]);
            }
            line(&[(13.0, 1.5), (13.0, 14.5)]);
            line(&[(4.0, 8.0), (10.0, 8.0)]);
            line(&[(7.5, 5.5), (10.0, 8.0), (7.5, 10.5)]);
            corner();
        }
        Glyph::Select => icons::paint(painter, rect, Icon::Selection, color, color),
        Glyph::TextNote => {
            line(&[(2.0, 15.0), (8.0, 1.0), (14.0, 15.0)]);
            line(&[(4.5, 10.0), (11.5, 10.0)]);
        }
        Glyph::TableDisplay => {
            outline(r(0.5, 3.0, 15.5, 13.0), 0);
            fill(r(3.0, 5.0, 5.0, 11.0));
            fill(r(7.0, 5.5, 9.0, 7.5));
            fill(r(10.5, 6.5, 12.5, 8.5));
        }
        Glyph::SelectTables => {
            outline(r(1.0, 1.5, 13.0, 12.0), 1);
            fill(r(1.0, 1.5, 13.0, 3.5));
            painter.add(Shape::convex_polygon(
                vec![p(8.0, 6.5), p(15.0, 10.0), p(11.5, 11.0), p(10.0, 14.5)],
                color,
                Stroke::NONE,
            ));
        }
        Glyph::ZoomIn => magnifier(true),
        Glyph::ZoomOut => magnifier(false),
        Glyph::PageBreaks => {
            outline(r(0.5, 2.0, 15.5, 14.0), 0);
            for y in [3.5, 6.5, 9.5, 12.5] {
                line(&[(8.0, y), (8.0, y + 1.5)]);
            }
        }
        Glyph::PageSetup => icons::paint(painter, rect, Icon::PageSetup, color, color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::SchemaMatch;

    /// A file with 2,000 key fields on each side of one relationship of
    /// 2,000 predicates: every box stays within `LIMIT`, the hidden keys
    /// meet the count row, and the line meets every key row drawn.
    #[test]
    fn hostile_key_counts_stay_bounded_and_every_predicate_meets_its_row() {
        let table = |id| SchemaTable {
            id,
            name: format!("Table {id}"),
            comment: String::new(),
            fields: (1..=2_000)
                .map(|field| SchemaField {
                    id: field,
                    name: format!("Key {field}"),
                    kind: SchemaFieldKind::Normal,
                    data_type: FieldKind::Text,
                    comment: String::new(),
                    unique: false,
                    unstored: false,
                    options: None,
                    calculation: None,
                    summary: None,
                    definition: None,
                })
                .collect(),
        };
        let occurrence = |id, table_id, x| SchemaOccurrence {
            id,
            name: format!("Occurrence {id}"),
            table_id: Some(table_id),
            graph: Some(OccurrenceBox::new_at(x, 20.0)),
        };
        let schema = DatabaseSchema {
            tables: vec![table(0x8002), table(0x8003)],
            occurrences: vec![occurrence(1, 0x8002, 20.0), occurrence(2, 0x8003, 300.0)],
            relationships: vec![SchemaRelationship {
                id: 1,
                left_occurrence: 1,
                right_occurrence: 2,
                matches: (1..=2_000)
                    .map(|field| SchemaMatch {
                        left_field: field,
                        right_field: field,
                        comparison: "=".into(),
                        operator: 0,
                    })
                    .collect(),
                options: 0,
                orders: [None, None],
            }],
            ..Default::default()
        };
        let graph = Graph::new(&schema);
        for node in &graph.nodes {
            assert_eq!(node.key_row_count(), MAX_KEY_ROWS);
            assert!(node.rect().height() <= LIMIT, "{}", node.rect().height());
            // Field ids start at 1: field n is row n - 1, and the fields
            // from MAX_KEY_ROWS on share the count row.
            let count_row = Node::key_row_y(MAX_KEY_ROWS - 1);
            let last = MAX_KEY_ROWS as u64 - 1;
            assert_eq!(node.anchor_y(last), Node::key_row_y(MAX_KEY_ROWS - 2));
            assert_eq!(node.anchor_y(last + 1), count_row);
            assert_eq!(node.anchor_y(2_000), count_row);
        }
        let route = graph.route(&graph.links[0]);
        for rows in &route.rows {
            assert_eq!(rows.len(), MAX_KEY_ROWS);
        }
    }
}
