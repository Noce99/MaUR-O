//! What a network *is*, written down for a person to look at.
//!
//! `architecture.txt` in a run folder is burn's `Display` of a module: its
//! fields and their layer parameters, and nothing of what happens between
//! them. The skip connections of the U-Net, the concatenations, the two heads
//! of the tree and every tensor shape live in a hand-written `forward`, so no
//! tool can read them off the module. A [`NetDescriptor`] is that missing
//! half, declared by hand next to each `forward` and checked against it by a
//! test (a forward pass reports the shape of every tensor it makes, and each
//! one is compared with what the descriptor says).
//!
//! # Shape of a descriptor
//!
//! A descriptor is a **tree of [`Node`]s**, and the nesting is what a viewer
//! zooms through: the root is the whole network, its children are the blocks
//! a person would name (an encoder, a head), theirs are the blocks those are
//! built of (a double convolution), and the leaves are single layers. Every
//! node says what it holds ([`Node::params`]) and how many weights it has
//! ([`Node::param_count`], a parent's being the sum of its children's).
//!
//! The dataflow between a node's children is its [`Node::edges`]. An
//! [`Edge`] names the child it leaves and the child it enters, and the shape
//! of the tensor that travels along it. A node's own boundary is written
//! `@in` and `@out`, with an optional port after a colon when there are
//! several (`@in:skip`, `@out:curves`); a child's port is written the same
//! way, `concat:skip`. A shape is a list of [`Dim`]s: numbers, or small
//! expressions over symbols such as `H/2`, so one descriptor holds for every
//! batch and image size.
//!
//! # Identity
//!
//! [`NetDescriptor::canonical_hash`] is a hash of the architecture alone:
//! scope, name, configuration and tree, written with sorted keys and nothing
//! that varies from one run to the next. The version, the date and the commit
//! are [`Meta`] and outside it, so asking for a descriptor twice gives the
//! same hash, and a change to it means a change to the network. That is what
//! [`crate::net::registry`] uses to decide whether there is a new version.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The version of the descriptor *format*, bumped when a reader of an old
/// file would misread it.
pub const SCHEMA_VERSION: u32 = 1;

/// One dimension of a tensor's shape: a number, or an expression over
/// symbols such as `B`, `H` and `W` (`"H/2"`, `"C+3"`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dim {
    /// Known whatever the input is.
    Int(u64),
    /// Depends on a symbol; see [`Dim::eval`] for the grammar.
    Expr(String),
}

impl From<&str> for Dim {
    fn from(text: &str) -> Dim {
        text.trim()
            .parse::<u64>()
            .map(Dim::Int)
            .unwrap_or_else(|_| Dim::Expr(text.trim().to_string()))
    }
}

impl Dim {
    /// The value of this dimension once every symbol has one, or `None` if
    /// the expression names a symbol not given, divides inexactly, or is
    /// malformed.
    ///
    /// The grammar is integers, identifiers, `+ - * /` and parentheses, with
    /// the usual precedence. Division must be exact, because a dimension that
    /// is not a whole number is a descriptor that is wrong.
    pub fn eval(&self, symbols: &[(&str, i64)]) -> Option<i64> {
        match self {
            Dim::Int(n) => i64::try_from(*n).ok(),
            Dim::Expr(text) => {
                let mut parser = Expr {
                    text: text.as_bytes(),
                    at: 0,
                    symbols,
                };
                let value = parser.sum()?;
                parser.skip_space();
                (parser.at == parser.text.len()).then_some(value)
            }
        }
    }
}

/// A recursive-descent evaluator for [`Dim::eval`].
struct Expr<'a> {
    text: &'a [u8],
    at: usize,
    symbols: &'a [(&'a str, i64)],
}

impl Expr<'_> {
    fn skip_space(&mut self) {
        while self.text.get(self.at) == Some(&b' ') {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.skip_space();
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> Option<i64> {
        let mut value = self.product()?;
        loop {
            if self.eat(b'+') {
                value = value.checked_add(self.product()?)?;
            } else if self.eat(b'-') {
                value = value.checked_sub(self.product()?)?;
            } else {
                return Some(value);
            }
        }
    }

    fn product(&mut self) -> Option<i64> {
        let mut value = self.atom()?;
        loop {
            if self.eat(b'*') {
                value = value.checked_mul(self.atom()?)?;
            } else if self.eat(b'/') {
                let divisor = self.atom()?;
                if divisor == 0 || value % divisor != 0 {
                    return None;
                }
                value /= divisor;
            } else {
                return Some(value);
            }
        }
    }

    fn atom(&mut self) -> Option<i64> {
        if self.eat(b'(') {
            let value = self.sum()?;
            return self.eat(b')').then_some(value);
        }
        self.skip_space();
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.at += 1;
        }
        let word = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        if word.is_empty() {
            return None;
        }
        match word.parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => self
                .symbols
                .iter()
                .find(|(name, _)| *name == word)
                .map(|(_, value)| *value),
        }
    }
}

/// The tensor which travels from one place in a node to another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Where it leaves: a child's id (`child:port` for one of several outputs),
    /// or `@in` / `@in:port` for the node's own input.
    pub from: String,
    /// Where it goes: a child's id (`child:port` when it has several inputs),
    /// or `@out` / `@out:port` for the node's own output.
    pub to: String,
    /// Its shape, symbolic where it depends on the input.
    pub shape: Vec<Dim>,
    /// Its element type.
    pub dtype: String,
}

/// One block of a network: the whole of it, a named part, or a single layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Names it among its siblings: no `/`, `@` or `:` in it. The path from
    /// the root, joined with `/`, names it in the whole network.
    pub id: String,
    /// What sort of block: `model`, `group`, `double_conv`, `conv2d`,
    /// `batch_norm`, `relu`, `max_pool2d`, `linear`, … A viewer picks a look
    /// by it.
    pub kind: String,
    /// What a person reads on it.
    pub label: String,
    /// One line for when it is drawn too small to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// What it is made of: kernel sizes, channel counts, strides.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
    /// How many values it stores, children included: the weights, and a batch
    /// norm's running statistics. What burn's `num_params` says of the module.
    pub param_count: u64,
    /// What it is built of, in reading order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// How its children's tensors flow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
}

impl Node {
    /// A block with nothing in it yet.
    pub fn new(id: &str, kind: &str, label: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: kind.to_string(),
            label: label.to_string(),
            summary: None,
            params: BTreeMap::new(),
            param_count: 0,
            children: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Sets the one-line summary.
    pub fn summary(mut self, text: &str) -> Node {
        self.summary = Some(text.to_string());
        self
    }

    /// Records one of the numbers it is made with.
    pub fn param(mut self, key: &str, value: impl Into<Value>) -> Node {
        self.params.insert(key.to_string(), value.into());
        self
    }

    /// Adds `count` to its weights: for a layer, what it holds itself.
    pub fn weights(mut self, count: u64) -> Node {
        self.param_count += count;
        self
    }

    /// Appends a child, and its weights to this node's.
    pub fn child(mut self, child: Node) -> Node {
        self.param_count += child.param_count;
        self.children.push(child);
        self
    }

    /// Appends the tensor of `shape` flowing from `from` to `to`, as `f32`.
    pub fn edge(mut self, from: &str, to: &str, shape: &[&str]) -> Node {
        self.edges.push(Edge {
            from: from.to_string(),
            to: to.to_string(),
            shape: shape.iter().map(|d| Dim::from(*d)).collect(),
            dtype: "f32".to_string(),
        });
        self
    }
}

/// Where a descriptor came from; outside the [`NetDescriptor::canonical_hash`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// When it was written, RFC 3339 UTC.
    #[serde(default)]
    pub generated_at: String,
    /// The commit of the code it was written from, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
}

/// A whole network, ready to be written as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetDescriptor {
    /// The version of this file format, [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// What the network is for: `image_to_omap`.
    pub scope: String,
    /// Which network of that scope: `unet`, `tree`.
    pub name: String,
    /// `major.minor`, given by [`crate::net::registry`] when the descriptor is
    /// registered; empty before.
    #[serde(default)]
    pub version: String,
    /// What it was built with: the fields of its configuration which change
    /// its shape.
    pub config: BTreeMap<String, Value>,
    /// The network, from its input to its output.
    pub root: Node,
    /// Date and commit.
    #[serde(default)]
    pub meta: Meta,
}

impl NetDescriptor {
    /// An unversioned descriptor of `root`.
    pub fn new(
        scope: &str,
        name: &str,
        config: BTreeMap<String, Value>,
        root: Node,
    ) -> NetDescriptor {
        NetDescriptor {
            schema_version: SCHEMA_VERSION,
            scope: scope.to_string(),
            name: name.to_string(),
            version: String::new(),
            config,
            root,
            meta: Meta::default(),
        }
    }

    /// How many values the whole network stores; see [`Node::param_count`].
    pub fn total_params(&self) -> u64 {
        self.root.param_count
    }

    /// How many values the tensors of one pass hold, the symbols of their
    /// shapes given (`B`, `H`, `W`, …): the input, and every leaf layer's
    /// output once, however many places it goes to. Which is what a pass
    /// keeps for its backward one, and so most of what training it costs in
    /// memory beyond the weights. `None` where a shape names a symbol not
    /// given or divides inexactly.
    pub fn activation_count(&self, symbols: &[(&str, i64)]) -> Option<u64> {
        fn size(edge: &Edge, symbols: &[(&str, i64)]) -> Option<u64> {
            edge.shape.iter().try_fold(1u64, |n, dim| {
                let d = u64::try_from(dim.eval(symbols)?).ok()?;
                n.checked_mul(d)
            })
        }
        fn walk(node: &Node, symbols: &[(&str, i64)]) -> Option<u64> {
            let mut total = 0u64;
            let mut counted: Vec<&str> = Vec::new();
            for edge in &node.edges {
                let from = edge.from.split(':').next().unwrap_or("");
                let leaf = node
                    .children
                    .iter()
                    .any(|c| c.id == from && c.children.is_empty());
                if leaf && !counted.contains(&from) {
                    counted.push(from);
                    total += size(edge, symbols)?;
                }
            }
            for child in &node.children {
                total += walk(child, symbols)?;
            }
            Some(total)
        }
        let input = self
            .root
            .edges
            .iter()
            .find(|e| e.from.starts_with("@in"))
            .map_or(Some(0), |e| size(e, symbols))?;
        Some(input + walk(&self.root, symbols)?)
    }

    /// A hash of the architecture: the same for the same network and
    /// different for a different one, and unaffected by the version and the
    /// [`Meta`]. 16 hex digits of FNV-1a over the sorted-key JSON — enough to
    /// notice a change, which is all it is for.
    pub fn canonical_hash(&self) -> String {
        let value = json!({
            "schema_version": self.schema_version,
            "scope": self.scope,
            "name": self.name,
            "config": self.config,
            "root": self.root,
        });
        let mut text = String::new();
        canonical_json(&value, &mut text);
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// Every edge of the tree with the name its source tensor goes by in a
    /// forward pass's report: the node's path and the edge's `from`,
    /// `path/child` for a child and `path@in` for the node's own input.
    /// What a test compares the shapes with.
    pub fn edges_with_keys(&self) -> Vec<(String, &Edge)> {
        fn walk<'a>(node: &'a Node, path: &str, out: &mut Vec<(String, &'a Edge)>) {
            for edge in &node.edges {
                let key = if edge.from.starts_with('@') {
                    format!("{path}{}", edge.from)
                } else {
                    format!("{path}/{}", edge.from)
                };
                out.push((key, edge));
            }
            for child in &node.children {
                walk(child, &format!("{path}/{}", child.id), out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &self.root.id, &mut out);
        out
    }

    /// What is wrong with the descriptor as a document, one line each: empty
    /// ids, ids twice among siblings, edges which start or end nowhere,
    /// weights which do not add up.
    pub fn problems(&self) -> Vec<String> {
        fn walk(node: &Node, path: &str, out: &mut Vec<String>) {
            let mut seen = std::collections::BTreeSet::new();
            for child in &node.children {
                if child.id.is_empty() || child.id.contains(['/', '@', ':']) {
                    out.push(format!("{path}: bad id {:?}", child.id));
                }
                if !seen.insert(child.id.as_str()) {
                    out.push(format!("{path}: two children called {:?}", child.id));
                }
            }
            let child_weights: u64 = node.children.iter().map(|c| c.param_count).sum();
            if !node.children.is_empty() && node.param_count < child_weights {
                out.push(format!("{path}: fewer weights than its children"));
            }
            let ends = |name: &str, boundary: &str| -> bool {
                let (head, _) = name.split_once(':').unwrap_or((name, ""));
                head == boundary || node.children.iter().any(|c| c.id == head)
            };
            for edge in &node.edges {
                if !ends(&edge.from, "@in") {
                    out.push(format!("{path}: edge starts at {:?}", edge.from));
                }
                if !ends(&edge.to, "@out") {
                    out.push(format!("{path}: edge ends at {:?}", edge.to));
                }
                if edge.from.starts_with("@out") || edge.to.starts_with("@in") {
                    out.push(format!(
                        "{path}: edge {} -> {} runs the wrong way",
                        edge.from, edge.to
                    ));
                }
            }
            for child in &node.children {
                walk(child, &format!("{path}/{}", child.id), out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &self.root.id, &mut out);
        out
    }
}

/// Writes `value` with every object's keys in sorted order and no spaces.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Weight counts and ready-made leaves for the layers networks here are built
/// of, so each `describe` reads like the `forward` it sits beside.
pub mod layer {
    use super::Node;

    /// Weights of a `kernel` by `kernel` convolution.
    pub fn conv2d_weights(from: usize, to: usize, kernel: usize, bias: bool) -> u64 {
        (from * to * kernel * kernel + if bias { to } else { 0 }) as u64
    }

    /// A convolution which keeps the picture the size it is, or
    /// strides it down: `from` channels to `to`.
    pub fn conv2d(id: &str, from: usize, to: usize, kernel: usize, bias: bool) -> Node {
        Node::new(id, "conv2d", &format!("Conv {kernel}×{kernel}"))
            .summary(&format!("{from}→{to} channels"))
            .param("in_channels", from)
            .param("out_channels", to)
            .param("kernel", kernel)
            .param("stride", 1)
            .param("padding", kernel / 2)
            .param("bias", bias)
            .weights(conv2d_weights(from, to, kernel, bias))
    }

    /// Batch normalization over `channels`: a scale and a shift each, and the
    /// running mean and variance it keeps for when it is not normalizing by
    /// the batch. Burn counts all four as parameters, and so does this.
    pub fn batch_norm(id: &str, channels: usize) -> Node {
        Node::new(id, "batch_norm", "BatchNorm")
            .param("channels", channels)
            .weights(4 * channels as u64)
    }

    /// A rectifier.
    pub fn relu(id: &str) -> Node {
        Node::new(id, "relu", "ReLU")
    }

    /// A `2×2` max pool with stride two.
    pub fn max_pool(id: &str) -> Node {
        Node::new(id, "max_pool2d", "MaxPool 2×2")
            .summary("halves height and width")
            .param("kernel", 2)
            .param("stride", 2)
    }

    /// A dense layer.
    pub fn linear(id: &str, from: usize, to: usize) -> Node {
        Node::new(id, "linear", "Linear")
            .summary(&format!("{from}→{to}"))
            .param("in_features", from)
            .param("out_features", to)
            .weights((from * to + to) as u64)
    }

    /// A transposed convolution which doubles the picture
    /// (kernel two, stride two).
    pub fn upsample(id: &str, from: usize, to: usize) -> Node {
        Node::new(id, "conv_transpose2d", "ConvTranspose 2×2")
            .summary(&format!("doubles height and width, {from}→{to}"))
            .param("in_channels", from)
            .param("out_channels", to)
            .param("kernel", 2)
            .param("stride", 2)
            .weights((from * to * 4 + to) as u64)
    }
}

/// How many opaque areas the registered descriptors are drawn for. A network's
/// shape depends on its symbol set, so the history needs one fixed: this is
/// the ground cover of `simple_line_and_colors.omap`, the set the sample
/// datasets are drawn from. Changing it is a change to every network, and
/// makes a new version of each.
pub const REFERENCE_CLASSES: usize = 11;

/// How many line symbols the registered U-Net's lines layer has a channel
/// for: the lines of the same set, `simple_line_and_colors.omap`. The
/// registered network is the one `mauro net train` builds on a dataset made
/// with line labels, which is what the sample datasets are.
pub const REFERENCE_LINES: usize = 6;

/// How many point symbols the registered U-Net's points layer has a heatmap
/// for: the ten of `symbols_sets/simple_ISSprOM_points.omap`, the set the
/// point datasets (`mauro net dataset --content areas-lines-points`) are
/// drawn from.
pub const REFERENCE_POINTS: usize = 10;

/// Whether the registered U-Net says which way its points are turned: it
/// does, being the one `mauro net train` builds on a point dataset, every one
/// of which now labels the angle of its points too (see
/// [`crate::point_truth::PointTruth::angles`]).
pub const REFERENCE_POINT_ANGLES: bool = true;

/// Whether the registered U-Net has a scale head: it does, being the one
/// `mauro net train` builds on a dataset drawn over a range of densities
/// (`mauro net dataset --min-resolution/--max-resolution`), which is what
/// it takes for the network to say how dense a picture is.
pub const REFERENCE_SCALE_HEAD: bool = true;

/// The networks the registry follows, in the order a listing shows them.
pub const BLESSED: [&str; 2] = ["unet", "tree"];

/// The descriptor of the network called `name` as `mauro net train` builds it
/// by default (the command line's own defaults, for [`REFERENCE_CLASSES`]),
/// or `None` if there is no such network.
pub fn blessed(name: &str) -> Option<NetDescriptor> {
    use crate::net::tree::TreeNetConfig;
    use crate::net::unet::UNetConfig;
    match name {
        "unet" => Some(
            UNetConfig::new(REFERENCE_CLASSES)
                .with_lines(REFERENCE_LINES)
                .with_points(REFERENCE_POINTS)
                .with_point_angles(REFERENCE_POINT_ANGLES)
                .with_scale_head(REFERENCE_SCALE_HEAD)
                .describe(),
        ),
        // `TreeNetConfig`'s own default is six control points; training
        // builds it with eight, which is what a run is.
        "tree" => Some(
            TreeNetConfig::new(REFERENCE_CLASSES)
                .with_control_points(8)
                .describe(),
        ),
        _ => None,
    }
}

/// The shape of `symbol` after `level` halvings: `H`, `H/2`, `H/4`, …
pub fn halved(symbol: &str, level: usize) -> String {
    if level == 0 {
        symbol.to_string()
    } else {
        format!("{symbol}/{}", 1usize << level)
    }
}

/// What a test needs to hold a descriptor to a real forward pass: every edge's
/// shape, with `symbols` filled in, must equal the shape the pass reported
/// for the tensor the edge starts at, and every edge must have been reported.
#[cfg(test)]
pub(crate) fn assert_matches_forward(
    descriptor: &NetDescriptor,
    reported: &[(String, Vec<usize>)],
    symbols: &[(&str, i64)],
) {
    let mut by_key: BTreeMap<&str, &Vec<usize>> = BTreeMap::new();
    for (key, dims) in reported {
        by_key.insert(key.as_str(), dims);
    }
    let mut checked = 0;
    for (key, edge) in descriptor.edges_with_keys() {
        let actual = by_key
            .get(key.as_str())
            .unwrap_or_else(|| panic!("the forward pass did not report {key}"));
        let declared: Vec<Option<i64>> = edge.shape.iter().map(|d| d.eval(symbols)).collect();
        let actual: Vec<Option<i64>> = actual.iter().map(|&d| Some(d as i64)).collect();
        assert_eq!(
            declared, actual,
            "edge {} -> {} out of {key}: {:?}",
            edge.from, edge.to, edge.shape
        );
        checked += 1;
    }
    assert!(checked > 0, "a descriptor with no edges proves nothing");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> NetDescriptor {
        let root = Node::new("net", "model", "Net")
            .child(layer::conv2d("conv", 3, 8, 3, false))
            .child(layer::relu("relu"))
            .edge("@in", "conv", &["B", "3", "H", "W"])
            .edge("conv", "relu", &["B", "8", "H", "W"])
            .edge("relu", "@out", &["B", "8", "H", "W"]);
        NetDescriptor::new("image_to_omap", "tiny", BTreeMap::new(), root)
    }

    #[test]
    fn expressions_evaluate_with_precedence_and_exact_division() {
        let symbols = [("H", 64), ("C", 11)];
        let eval = |text: &str| Dim::from(text).eval(&symbols);
        assert_eq!(eval("H"), Some(64));
        assert_eq!(eval("H/4"), Some(16));
        assert_eq!(eval("C+3"), Some(14));
        assert_eq!(eval("2*C+1"), Some(23));
        assert_eq!(eval("(C+1)*2"), Some(24));
        assert_eq!(eval("128"), Some(128));
        // Not a whole number, an unknown symbol, and nonsense.
        assert_eq!(eval("C/2"), None);
        assert_eq!(eval("W"), None);
        assert_eq!(eval("H/"), None);
        assert_eq!(eval("H H"), None);
    }

    #[test]
    fn a_dim_is_a_number_in_json_when_it_can_be() {
        let shape = vec![Dim::from("B"), Dim::from("16"), Dim::from("H/2")];
        assert_eq!(serde_json::to_string(&shape).unwrap(), r#"["B",16,"H/2"]"#);
        let back: Vec<Dim> = serde_json::from_str(r#"["B",16,"H/2"]"#).unwrap();
        assert_eq!(back, shape);
    }

    #[test]
    fn the_hash_is_stable_and_ignores_version_and_meta() {
        let a = tiny();
        let mut b = tiny();
        b.version = "0.7".to_string();
        b.meta = Meta {
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            git_commit: Some("abc123".to_string()),
        };
        assert_eq!(a.canonical_hash(), b.canonical_hash());
        assert_eq!(a.canonical_hash(), tiny().canonical_hash());
        assert_eq!(a.canonical_hash().len(), 16);
    }

    #[test]
    fn the_hash_follows_the_architecture() {
        let a = tiny();
        let mut wider = tiny();
        wider.root.children[0] = layer::conv2d("conv", 3, 16, 3, false);
        assert_ne!(a.canonical_hash(), wider.canonical_hash());

        let mut renamed = tiny();
        renamed.name = "other".to_string();
        assert_ne!(a.canonical_hash(), renamed.canonical_hash());
    }

    #[test]
    fn a_descriptor_survives_a_round_trip_through_json() {
        let a = tiny();
        let text = serde_json::to_string_pretty(&a).unwrap();
        let b: NetDescriptor = serde_json::from_str(&text).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn weights_add_up_through_the_tree() {
        let a = tiny();
        assert_eq!(a.total_params(), 3 * 8 * 9);
    }

    #[test]
    fn problems_names_what_is_wrong() {
        assert!(tiny().problems().is_empty(), "{:?}", tiny().problems());

        let mut twice = tiny();
        twice.root = twice.root.child(layer::relu("relu"));
        assert!(twice.problems().iter().any(|p| p.contains("two children")));

        let mut nowhere = tiny();
        nowhere.root = nowhere.root.edge("relu", "ghost", &["B"]);
        assert!(nowhere.problems().iter().any(|p| p.contains("ghost")));

        let mut backwards = tiny();
        backwards.root = backwards.root.edge("@out", "relu", &["B"]);
        assert!(!backwards.problems().is_empty());
    }

    #[test]
    fn every_blessed_network_is_sound_and_stable() {
        for name in BLESSED {
            let a = blessed(name).unwrap();
            assert_eq!(a.name, name);
            assert_eq!(a.problems(), Vec::<String>::new(), "{name}");
            assert_eq!(a.canonical_hash(), blessed(name).unwrap().canonical_hash());
            assert!(a.total_params() > 0);
        }
        assert!(blessed("nope").is_none());
    }

    #[test]
    fn edge_keys_name_the_tensor_they_start_at() {
        let keys: Vec<String> = tiny()
            .edges_with_keys()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, ["net@in", "net/conv", "net/relu"]);
    }
}
