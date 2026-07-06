use super::*;

const MAIN_TF: &str = r#"
resource "aws_sqs_queue" "orders" {
  name = "orders-queue"
}

resource "aws_lambda_function" "fulfill" {
  function_name = "fulfill-orders"
  role          = aws_iam_role.lambda_role.arn
}

resource "aws_lambda_event_source_mapping" "orders_to_fulfill" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}

resource "aws_iam_role" "lambda_role" {
  name = "fulfill-role"
}

resource "aws_iam_role_policy" "fulfill_policy" {
  role   = aws_iam_role.lambda_role.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action   = ["sqs:ReceiveMessage", "sqs:DeleteMessage"]
      Effect   = "Allow"
      Resource = aws_sqs_queue.orders.arn
    }]
  })
}

resource "aws_sns_topic_subscription" "alerts" {
  topic_arn = aws_sns_topic.alerts.arn
  protocol  = "sqs"
  endpoint  = aws_sqs_queue.orders.arn

  depends_on = [aws_sqs_queue.orders]
}

resource "aws_sns_topic" "alerts" {
  name = "alerts"
}

module "vpc" {
  source = "./modules/vpc"
}

data "aws_ami" "app" {
  owners = ["self"]
}

resource "aws_instance" "app" {
  ami       = data.aws_ami.app.id
  subnet_id = module.vpc.private_subnet_id
  count     = var.instance_count
}
"#;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "qwtm/infra",
        commit: "def456",
    }
}

fn edge_pairs<'a>(ex: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    ex.edges
        .iter()
        .filter(|e| e.label == label)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

#[test]
fn resources_data_and_modules_become_resource_nodes() {
    // AC-0007: resource DAG built from HCL.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let ids: Vec<_> = ex.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"res:aws_sqs_queue.orders"));
    assert!(ids.contains(&"res:data.aws_ami.app"));
    assert!(ids.contains(&"res:module.vpc"));
    let queue = ex
        .nodes
        .iter()
        .find(|n| n.id == "res:aws_sqs_queue.orders")
        .unwrap();
    assert_eq!(queue.props["provider"], "aws");
    assert_eq!(queue.props["type"], "aws_sqs_queue");
}

#[test]
fn interpolation_references_build_the_dag() {
    // AC-0007: refs like data.aws_ami.app.id / module.vpc.x become edges;
    // var.* / count.* never do.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let refs = edge_pairs(&ex, "REFERENCES");
    assert!(refs.contains(&("res:aws_instance.app", "res:data.aws_ami.app")));
    assert!(refs.contains(&("res:aws_instance.app", "res:module.vpc")));
    assert!(refs.contains(&(
        "res:aws_lambda_function.fulfill",
        "res:aws_iam_role.lambda_role"
    )));
    assert!(
        !ex.edges
            .iter()
            .any(|e| e.dst.contains("var.") || e.dst.contains("count."))
    );
}

#[test]
fn depends_on_is_distinct_from_references() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let deps = edge_pairs(&ex, "DEPENDS_ON");
    assert!(deps.contains(&(
        "res:aws_sns_topic_subscription.alerts",
        "res:aws_sqs_queue.orders"
    )));
}

#[test]
fn capability_registry_emits_triggers_deterministically() {
    // AC-0008: the event source mapping mediates SQS -> Lambda TRIGGERS.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let triggers = edge_pairs(&ex, "TRIGGERS");
    assert_eq!(
        triggers,
        vec![(
            "res:aws_sqs_queue.orders",
            "res:aws_lambda_function.fulfill"
        )]
    );
    let edge = ex.edges.iter().find(|e| e.label == "TRIGGERS").unwrap();
    assert_eq!(
        edge.props["via"],
        "res:aws_lambda_event_source_mapping.orders_to_fulfill"
    );
    assert_eq!(edge.props["registry"], registry::REGISTRY_VERSION);
}

#[test]
fn capability_registry_emits_subscribes() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let subs = edge_pairs(&ex, "SUBSCRIBES");
    assert_eq!(
        subs,
        vec![("res:aws_sqs_queue.orders", "res:aws_sns_topic.alerts")]
    );
}

#[test]
fn iam_policy_grants_reference_target_resources_with_actions() {
    // AC-0008 (GRANTS) — feeds the security view (US-0015).
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let grants = edge_pairs(&ex, "GRANTS");
    assert_eq!(
        grants,
        vec![(
            "res:aws_iam_role_policy.fulfill_policy",
            "res:aws_sqs_queue.orders"
        )]
    );
    let edge = ex.edges.iter().find(|e| e.label == "GRANTS").unwrap();
    let actions: Vec<String> = serde_json::from_value(edge.props["actions"].clone()).unwrap();
    assert_eq!(actions, vec!["sqs:DeleteMessage", "sqs:ReceiveMessage"]);
}

#[test]
fn every_fact_carries_confirmed_t0_provenance_with_spans() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    for props in ex
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(ex.edges.iter().map(|e| &e.props))
    {
        let prov = props.get("prov").expect("prov present");
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.iac-terraform");
        let ev = &prov["evidence"][0];
        assert_eq!(ev["path"], "main.tf");
        assert!(ev["byte_end"].as_u64().unwrap() > ev["byte_start"].as_u64().unwrap());
    }
}

#[test]
fn evidence_span_covers_the_declaring_block() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let mapping = ex
        .nodes
        .iter()
        .find(|n| n.id == "res:aws_lambda_event_source_mapping.orders_to_fulfill")
        .unwrap();
    let ev = &mapping.props["prov"]["evidence"][0];
    let span = &MAIN_TF
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(span.contains(r#"resource "aws_lambda_event_source_mapping" "orders_to_fulfill""#));
    assert!(span.contains("event_source_arn = aws_sqs_queue.orders.arn"));
}

#[test]
fn dir_walk_is_deterministic_and_skips_dot_terraform() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.tf"), MAIN_TF).unwrap();
    std::fs::create_dir_all(dir.path().join(".terraform/modules")).unwrap();
    std::fs::write(
        dir.path().join(".terraform/modules/cached.tf"),
        r#"resource "aws_s3_bucket" "cached" {}"#,
    )
    .unwrap();
    let a = extract_dir(dir.path(), &id()).unwrap();
    let b = extract_dir(dir.path(), &id()).unwrap();
    assert!(!a.nodes.iter().any(|n| n.id.contains("cached")));
    assert_eq!(
        serde_json::to_string(&a.nodes).unwrap(),
        serde_json::to_string(&b.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.edges).unwrap(),
        serde_json::to_string(&b.edges).unwrap()
    );
}

#[test]
fn syntax_errors_name_the_file() {
    let err = extract_source("resource \"broken\" {", "bad.tf", &id()).unwrap_err();
    assert!(err.to_string().contains("bad.tf"));
}
