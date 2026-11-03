//! Deterministic VerbNet XML import, separate from contextual frame matching.
//!
//! Original UTF-8 XML is the lossless layer: XML declaration, DTD, entity
//! spelling, whitespace, namespaces, unknown elements and attributes survive.
//! The owned tree exposes XML-normalized values with original byte positions.
//! The derived index interprets only class nesting, explicit membership, role
//! replacement by name, and cumulative inherited frames. It selects no sense.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

pub const SCHEMA: &str = "slopninja-verbnet-resource-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlSource {
    pub path: String,
    pub sha256: String,
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlAttribute {
    pub name: String,
    pub namespace: Option<String>,
    pub qualified_name: String,
    pub value: String,
    pub byte_span: [usize; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XmlNode {
    Element(XmlElement),
    Text {
        value: String,
        byte_span: [usize; 2],
    },
    Comment {
        value: String,
        byte_span: [usize; 2],
    },
    ProcessingInstruction {
        target: String,
        value: Option<String>,
        byte_span: [usize; 2],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlElement {
    pub name: String,
    pub namespace: Option<String>,
    pub qualified_name: String,
    pub attributes: Vec<XmlAttribute>,
    /// Namespace bindings in scope; their exact declarations remain in raw XML.
    pub namespaces: BTreeMap<String, String>,
    pub children: Vec<XmlNode>,
    pub byte_span: [usize; 2],
}

impl XmlElement {
    pub fn is(&self, name: &str) -> bool {
        self.namespace.is_none() && self.name == name
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.namespace.is_none() && a.name == name)
            .map(|a| a.value.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &XmlElement> {
        self.children.iter().filter_map(|n| match n {
            XmlNode::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn child(&self, name: &str) -> Option<&XmlElement> {
        self.elements().find(|e| e.is(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub source_path: String,
    pub source_sha256: String,
    pub declaring_class_id: String,
    pub byte_span: [usize; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub normalized_lemma: Option<String>,
    pub attributes: Vec<XmlAttribute>,
    pub origin: Origin,
    pub xml: XmlElement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Compact JSON [declaring class ID, zero-based own-frame ordinal].
    pub id: String,
    pub origin: Origin,
    pub xml: XmlElement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub origin: Origin,
    pub xml: XmlElement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Class {
    pub id: String,
    pub parent_id: Option<String>,
    /// Root through immediate parent, excluding this class.
    pub ancestor_ids: Vec<String>,
    pub origin: Origin,
    pub own_members: Vec<Member>,
    pub own_roles: Vec<Role>,
    pub own_frames: Vec<Frame>,
    /// Unique or byte-identical nearest declarations, represented once here.
    /// The complete declaration list below retains every duplicate and origin.
    pub effective_roles: BTreeMap<String, Role>,
    pub effective_role_declarations: BTreeMap<String, Vec<Role>>,
    /// Same-level declarations with differing XML; no representative selected.
    pub ambiguous_role_names: Vec<String>,
    /// Root-to-self declaration order; identical frames are not deduplicated.
    pub effective_frames: Vec<Frame>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MemberRef {
    pub class_id: String,
    pub member_ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupStatus {
    Found,
    MissingLemma,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lookup {
    pub normalized_lemma: Option<String>,
    pub status: LookupStatus,
    pub members: Vec<MemberRef>,
    pub open_world_unknown_use_possible: bool,
    pub sense_selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub schema: String,
    pub sources: Vec<XmlSource>,
    pub documents: BTreeMap<String, XmlElement>,
    pub classes: BTreeMap<String, Class>,
    pub lemma_index: BTreeMap<String, Vec<MemberRef>>,
}

/// Same supplied-lemma identity policy as lexical_context, without a Token.
/// Preserve spaces, underscores, punctuation and phrasal names exactly.
pub fn normalize_lemma(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(
            value
                .nfc()
                .collect::<String>()
                .to_lowercase()
                .nfc()
                .collect(),
        )
    }
}

impl Resource {
    pub fn from_sources(mut sources: Vec<XmlSource>) -> Result<Self> {
        ensure!(!sources.is_empty(), "No VerbNet XML sources supplied");
        sources.sort_by(|a, b| a.path.cmp(&b.path));
        let mut documents = BTreeMap::new();
        let mut classes = BTreeMap::new();
        for source in &sources {
            ensure!(!source.path.trim().is_empty(), "Empty XML source path");
            ensure!(
                hex::encode(Sha256::digest(source.xml.as_bytes())) == source.sha256,
                "XML source hash differs: {}",
                source.path
            );
            ensure!(
                !documents.contains_key(&source.path),
                "Duplicate XML source path: {}",
                source.path
            );
            // VerbNet declares an external DTD. Preserve it, but do not fetch
            // or resolve external entities. This parser performs no I/O.
            let parsed = roxmltree::Document::parse_with_options(
                &source.xml,
                roxmltree::ParsingOptions {
                    allow_dtd: true,
                    entity_resolver: None,
                    ..Default::default()
                },
            )
            .with_context(|| format!("Invalid XML: {}", source.path))?;
            let tree = element(parsed.root_element(), &source.xml);
            ensure!(
                tree.is("VNCLASS"),
                "Expected unqualified VNCLASS root: {}",
                source.path
            );
            import_class(&tree, source, None, &mut classes)?;
            validate_class_placement(&tree, source, &classes)?;
            documents.insert(source.path.clone(), tree);
        }
        let mut lemma_index: BTreeMap<String, Vec<MemberRef>> = BTreeMap::new();
        for (id, class) in &classes {
            for (member_ordinal, member) in class.own_members.iter().enumerate() {
                if let Some(lemma) = &member.normalized_lemma {
                    lemma_index
                        .entry(lemma.clone())
                        .or_default()
                        .push(MemberRef {
                            class_id: id.clone(),
                            member_ordinal,
                        });
                }
            }
        }
        Ok(Self {
            schema: SCHEMA.into(),
            sources,
            documents,
            classes,
            lemma_index,
        })
    }

    pub fn lookup(&self, supplied_lemma: &str) -> Lookup {
        let normalized_lemma = normalize_lemma(supplied_lemma);
        let members = normalized_lemma
            .as_ref()
            .and_then(|l| self.lemma_index.get(l))
            .cloned()
            .unwrap_or_default();
        let status = if normalized_lemma.is_none() {
            LookupStatus::MissingLemma
        } else if members.is_empty() {
            LookupStatus::Unknown
        } else {
            LookupStatus::Found
        };
        Lookup {
            normalized_lemma,
            status,
            members,
            open_world_unknown_use_possible: true,
            sense_selected: false,
        }
    }
}

fn element(node: roxmltree::Node<'_, '_>, source: &str) -> XmlElement {
    let range = node.range();
    let qualified_name = source[range.start + 1..range.end]
        .split(|c: char| c.is_ascii_whitespace() || matches!(c, '/' | '>'))
        .next()
        .unwrap()
        .to_string();
    let children = node
        .children()
        .map(|n| {
            let range = n.range();
            let byte_span = [range.start, range.end];
            match n.node_type() {
                roxmltree::NodeType::Element => XmlNode::Element(element(n, source)),
                roxmltree::NodeType::Text => XmlNode::Text {
                    value: n.text().unwrap_or("").into(),
                    byte_span,
                },
                roxmltree::NodeType::Comment => XmlNode::Comment {
                    value: n.text().unwrap_or("").into(),
                    byte_span,
                },
                roxmltree::NodeType::PI => {
                    let pi = n.pi().unwrap();
                    XmlNode::ProcessingInstruction {
                        target: pi.target.into(),
                        value: pi.value.map(str::to_string),
                        byte_span,
                    }
                }
                roxmltree::NodeType::Root => {
                    unreachable!("The document root cannot be an element child")
                }
            }
        })
        .collect();
    XmlElement {
        name: node.tag_name().name().into(),
        namespace: node.tag_name().namespace().map(str::to_string),
        qualified_name,
        attributes: node
            .attributes()
            .map(|a| {
                let range = a.range();
                XmlAttribute {
                    name: a.name().into(),
                    namespace: a.namespace().map(str::to_string),
                    qualified_name: source[a.range_qname()].into(),
                    value: a.value().into(),
                    byte_span: [range.start, range.end],
                }
            })
            .collect(),
        namespaces: node
            .namespaces()
            .map(|n| (n.name().unwrap_or("").into(), n.uri().into()))
            .collect(),
        children,
        byte_span: [range.start, range.end],
    }
}

fn unique_container<'a>(node: &'a XmlElement, name: &str) -> Result<Option<&'a XmlElement>> {
    let values = node.elements().filter(|e| e.is(name)).collect::<Vec<_>>();
    ensure!(values.len() <= 1, "Duplicate {name} container");
    Ok(values.into_iter().next())
}

fn validate_class_placement(
    node: &XmlElement,
    source: &XmlSource,
    classes: &BTreeMap<String, Class>,
) -> Result<()> {
    if node.is("VNCLASS") || node.is("VNSUBCLASS") {
        let id = required(node, "ID")?;
        ensure!(
            classes.get(id).is_some_and(|class| {
                class.origin.source_path == source.path && class.origin.byte_span == node.byte_span
            }),
            "Class declaration outside supported hierarchy: {id}"
        );
    }
    for child in node.elements() {
        validate_class_placement(child, source, classes)?;
    }
    Ok(())
}

fn declarations<'a>(
    node: &'a XmlElement,
    container: &str,
    item: &str,
) -> Result<Vec<&'a XmlElement>> {
    Ok(unique_container(node, container)?
        .into_iter()
        .flat_map(XmlElement::elements)
        .filter(|e| e.is(item))
        .collect())
}

fn required<'a>(node: &'a XmlElement, name: &str) -> Result<&'a str> {
    let value = node
        .attr(name)
        .with_context(|| format!("{} lacks {name}", node.name))?;
    ensure!(!value.trim().is_empty(), "{} has empty {name}", node.name);
    Ok(value)
}

fn origin(node: &XmlElement, source: &XmlSource, id: &str) -> Origin {
    Origin {
        source_path: source.path.clone(),
        source_sha256: source.sha256.clone(),
        declaring_class_id: id.into(),
        byte_span: node.byte_span,
    }
}

fn import_class(
    node: &XmlElement,
    source: &XmlSource,
    parent: Option<&str>,
    classes: &mut BTreeMap<String, Class>,
) -> Result<()> {
    let id = required(node, "ID")?.to_string();
    ensure!(!classes.contains_key(&id), "Duplicate class ID: {id}");
    let parent_class = parent.map(|id| classes.get(id).expect("Parent inserted before child"));
    let mut ancestor_ids = parent_class
        .map(|p| p.ancestor_ids.clone())
        .unwrap_or_default();
    if let Some(parent) = parent {
        ancestor_ids.push(parent.into());
    }
    let mut effective_role_declarations = parent_class
        .map(|p| p.effective_role_declarations.clone())
        .unwrap_or_default();
    let mut effective_frames = parent_class
        .map(|p| p.effective_frames.clone())
        .unwrap_or_default();
    let own_members = declarations(node, "MEMBERS", "MEMBER")?
        .into_iter()
        .map(|m| {
            // A present but blank name is retained as missing, without an index key.
            let name = m.attr("name").context("MEMBER lacks name")?.to_string();
            Ok(Member {
                normalized_lemma: normalize_lemma(&name),
                name,
                attributes: m.attributes.clone(),
                origin: origin(m, source, &id),
                xml: m.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let own_roles = declarations(node, "THEMROLES", "THEMROLE")?
        .into_iter()
        .map(|r| {
            let name = required(r, "type")?.to_string();
            Ok(Role {
                name,
                origin: origin(r, source, &id),
                xml: r.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut own_role_declarations: BTreeMap<String, Vec<Role>> = BTreeMap::new();
    for role in &own_roles {
        own_role_declarations
            .entry(role.name.clone())
            .or_default()
            .push(role.clone());
    }
    effective_role_declarations.extend(own_role_declarations);
    let mut effective_roles = BTreeMap::new();
    let mut ambiguous_role_names = Vec::new();
    for (name, roles) in &effective_role_declarations {
        let raw = |role: &Role| &source.xml[role.origin.byte_span[0]..role.origin.byte_span[1]];
        if roles.iter().all(|role| raw(role) == raw(&roles[0])) {
            effective_roles.insert(name.clone(), roles[0].clone());
        } else {
            ambiguous_role_names.push(name.clone());
        }
    }
    let own_frames = declarations(node, "FRAMES", "FRAME")?
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            Ok(Frame {
                id: serde_json::to_string(&(&id, i))?,
                origin: origin(f, source, &id),
                xml: f.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    effective_frames.extend(own_frames.iter().cloned());
    classes.insert(
        id.clone(),
        Class {
            id: id.clone(),
            parent_id: parent.map(str::to_string),
            ancestor_ids,
            origin: origin(node, source, &id),
            own_members,
            own_roles,
            own_frames,
            effective_roles,
            effective_role_declarations,
            ambiguous_role_names,
            effective_frames,
        },
    );
    for child in declarations(node, "SUBCLASSES", "VNSUBCLASS")? {
        import_class(child, source, Some(&id), classes)?;
    }
    // A misplaced structural declaration cannot silently vanish from the index.
    for child in node.elements() {
        if child.is("VNCLASS") || child.is("VNSUBCLASS") {
            bail!("Class declaration outside SUBCLASSES in {id}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(path: &str, xml: &str) -> XmlSource {
        XmlSource {
            path: path.into(),
            xml: xml.into(),
            sha256: hex::encode(Sha256::digest(xml.as_bytes())),
        }
    }
    fn inherited() -> XmlSource {
        source(
            "fixture.xml",
            r#"<!DOCTYPE VNCLASS SYSTEM "not-loaded.dtd">
<VNCLASS ID="root"><MEMBERS><MEMBER name="Root_Only" verbnet_key="root#1"/></MEMBERS>
<THEMROLES><THEMROLE type="Agent"><SELRESTRS><SELRESTR Value="+" type="animate"/></SELRESTRS></THEMROLE><THEMROLE type="Topic"/></THEMROLES>
<FRAMES><FRAME><SYNTAX><NP value="Agent"/><VERB/><NP value="Topic"><SYNRESTRS><SYNRESTR Value="+" type="that_comp"/></SYNRESTRS></NP></SYNTAX><SEMANTICS><PRED value="opaque"><ARGS><ARG type="ThemRole" value="?Topic"/></ARGS></PRED></SEMANTICS></FRAME></FRAMES>
<SUBCLASSES><VNSUBCLASS ID="child"><MEMBERS><MEMBER name="Tell" verbnet_key="tell#1"/><MEMBER name="Tell" verbnet_key="tell#2"/><MEMBER name=" Two_Words "/></MEMBERS>
<THEMROLES><THEMROLE type="Agent"><SELRESTRS logic="or"><SELRESTR Value="+" type="organization"/></SELRESTRS></THEMROLE></THEMROLES>
<FRAMES><FRAME><SYNTAX><NP value="Agent"/><VERB/></SYNTAX></FRAME></FRAMES>
<SUBCLASSES><VNSUBCLASS ID="grandchild"><MEMBERS><MEMBER name="tell" verbnet_key="tell#3"/></MEMBERS><FRAMES><FRAME><SYNTAX><NP value="Agent"/><VERB/></SYNTAX></FRAME></FRAMES></VNSUBCLASS></SUBCLASSES>
</VNSUBCLASS><VNSUBCLASS ID="sibling"><MEMBERS><MEMBER name="tell" verbnet_key="tell#4"/></MEMBERS></VNSUBCLASS></SUBCLASSES></VNCLASS>"#,
        )
    }

    #[test]
    fn inheritance_preserves_frames_and_nearest_role_override_without_member_expansion() {
        let r = Resource::from_sources(vec![inherited()]).unwrap();
        let c = &r.classes["grandchild"];
        assert_eq!(c.ancestor_ids, ["root", "child"]);
        assert_eq!(
            c.effective_frames
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            ["[\"root\",0]", "[\"child\",0]", "[\"grandchild\",0]"]
        );
        assert_eq!(
            c.effective_roles["Agent"].origin.declaring_class_id,
            "child"
        );
        assert_eq!(c.effective_roles["Topic"].origin.declaring_class_id, "root");
        assert_eq!(
            r.classes["sibling"].effective_roles["Agent"]
                .origin
                .declaring_class_id,
            "root"
        );
        assert_eq!(
            r.lookup("ROOT_ONLY").members,
            vec![MemberRef {
                class_id: "root".into(),
                member_ordinal: 0
            }]
        );
        for f in &c.effective_frames {
            let src = &r.sources[0];
            assert_eq!(
                &src.xml[f.origin.byte_span[0]..f.origin.byte_span[1]][..6],
                "<FRAME"
            );
            assert_eq!(f.origin.source_sha256, src.sha256);
        }
    }

    #[test]
    fn ambiguous_members_unicode_names_and_unknowns_are_not_collapsed() {
        let r = Resource::from_sources(vec![inherited(), source("unicode.xml", "<VNCLASS ID='unicode'><MEMBERS><MEMBER name='E&#x301;LAN'/><MEMBER name=' '/><MEMBER name='null'/></MEMBERS></VNCLASS>")]).unwrap();
        let found = r.lookup("TELL");
        assert_eq!(found.members.len(), 4);
        assert!(found.open_world_unknown_use_possible && !found.sense_selected);
        assert_eq!(r.lookup("ÉLAN").members.len(), 1);
        assert_eq!(r.lookup(" two_words ").members.len(), 1);
        assert_eq!(r.lookup("two_words").status, LookupStatus::Unknown);
        assert_eq!(r.lookup("unknown").status, LookupStatus::Unknown);
        assert_eq!(r.lookup(" \t").status, LookupStatus::MissingLemma);
        assert_eq!(r.lookup("null").status, LookupStatus::Found);
        assert_eq!(r.classes["unicode"].own_members[1].normalized_lemma, None);
    }

    #[test]
    fn unknown_xml_namespaces_text_and_entities_survive_with_exact_original_bytes() {
        let s = source(
            "opaque.xml",
            "<?xml version='1.0'?><!DOCTYPE VNCLASS [<!ENTITY sample 'A &amp; B'>]><VNCLASS ID='opaque' xmlns:x='urn:unknown'><FUTURE x:a='&quot;'><![CDATA[<raw>]]>&sample;<!-- comment --><?probe data?><x:NP value='different'/></FUTURE><MEMBERS><MEMBER name='opaque' surprise='yes'/></MEMBERS></VNCLASS>",
        );
        let r = Resource::from_sources(vec![s.clone()]).unwrap();
        assert_eq!(r.sources[0], s);
        let f = r.documents["opaque.xml"].child("FUTURE").unwrap();
        assert_eq!(f.attributes[0].qualified_name, "x:a");
        assert_eq!(f.attributes[0].namespace.as_deref(), Some("urn:unknown"));
        assert_eq!(f.attributes[0].value, "\"");
        assert!(
            f.children
                .iter()
                .any(|n| matches!(n,XmlNode::Text{value,..} if value.contains("<raw>A & B")))
        );
        assert!(
            f.children
                .iter()
                .any(|n| matches!(n,XmlNode::Comment{value,..} if value==" comment "))
        );
        assert!(
            f.children
                .iter()
                .any(|n| matches!(n,XmlNode::ProcessingInstruction{target,..} if target=="probe"))
        );
        assert!(f.child("NP").is_none());
        assert_eq!(f.elements().next().unwrap().qualified_name, "x:NP");
        let roundtrip: Resource = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(roundtrip, r);
    }

    #[test]
    fn order_is_deterministic_but_repeated_frame_declarations_survive() {
        let a = inherited();
        let b = source(
            "a.xml",
            "<VNCLASS ID='a'><MEMBERS><MEMBER name='tell'/></MEMBERS></VNCLASS>",
        );
        assert_eq!(
            Resource::from_sources(vec![a.clone(), b.clone()]).unwrap(),
            Resource::from_sources(vec![b, a]).unwrap()
        );
        let r = Resource::from_sources(vec![source(
            "x.xml",
            "<VNCLASS ID='x'><FRAMES><FRAME/><FRAME/></FRAMES></VNCLASS>",
        )])
        .unwrap();
        assert_eq!(r.classes["x"].effective_frames.len(), 2);
        assert_ne!(
            r.classes["x"].own_frames[0].id,
            r.classes["x"].own_frames[1].id
        );
    }

    #[test]
    fn malformed_inputs_and_duplicate_identity_fail_instead_of_dropping_evidence() {
        let mut wrong = inherited();
        wrong.xml.push(' ');
        assert!(Resource::from_sources(vec![wrong]).is_err());
        assert!(Resource::from_sources(vec![]).is_err());
        assert!(Resource::from_sources(vec![inherited(), inherited()]).is_err());
        for xml in [
            "<VNCLASS>",
            "<VNCLASS/>",
            "<x:VNCLASS xmlns:x='urn:x' ID='x'/>",
            "<VNCLASS ID='x'><SUBCLASSES><VNSUBCLASS ID='x'/></SUBCLASSES></VNCLASS>",
            "<VNCLASS ID='x'><MEMBERS/><MEMBERS/></VNCLASS>",
            "<VNCLASS ID='x'><MEMBERS><MEMBER/></MEMBERS></VNCLASS>",
            "<VNCLASS ID='x'><VNSUBCLASS ID='z'/></VNCLASS>",
            "<VNCLASS ID='x'><FUTURE><VNSUBCLASS ID='z'/></FUTURE></VNCLASS>",
            "<VNCLASS ID='x'><SUBCLASSES><VNCLASS ID='z'/></SUBCLASSES></VNCLASS>",
        ] {
            assert!(
                Resource::from_sources(vec![source("bad.xml", xml)]).is_err(),
                "{xml}"
            );
        }
    }

    #[test]
    fn external_entities_are_never_resolved() {
        let xml = "<!DOCTYPE VNCLASS [<!ENTITY external SYSTEM 'file:///never-open-this'>]><VNCLASS ID='x'><FUTURE>&external;</FUTURE></VNCLASS>";
        assert!(Resource::from_sources(vec![source("external.xml", xml)]).is_err());
    }

    #[test]
    fn duplicate_roles_remain_visible_and_conflicting_declarations_are_unresolved() {
        let xml = "<VNCLASS ID='x'><THEMROLES><THEMROLE type='Same'/><THEMROLE type='Same'/><THEMROLE type='Conflict' a='1'/><THEMROLE type='Conflict' a='2'/></THEMROLES><SUBCLASSES><VNSUBCLASS ID='child'><THEMROLES><THEMROLE type='Conflict' a='3'/></THEMROLES></VNSUBCLASS></SUBCLASSES></VNCLASS>";
        let resource = Resource::from_sources(vec![source("roles.xml", xml)]).unwrap();
        let root = &resource.classes["x"];
        assert_eq!(root.own_roles.len(), 4);
        assert_eq!(root.effective_role_declarations["Same"].len(), 2);
        assert_eq!(root.effective_role_declarations["Conflict"].len(), 2);
        assert!(root.effective_roles.contains_key("Same"));
        assert!(!root.effective_roles.contains_key("Conflict"));
        assert_eq!(root.ambiguous_role_names, ["Conflict"]);
        let child = &resource.classes["child"];
        assert!(child.ambiguous_role_names.is_empty());
        assert_eq!(child.effective_role_declarations["Same"].len(), 2);
        assert_eq!(child.effective_role_declarations["Conflict"].len(), 1);
        assert_eq!(child.effective_roles["Conflict"].xml.attr("a"), Some("3"));
    }
}
