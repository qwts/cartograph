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
    assert!(ids.contains(&"res:qwtm/infra@aws_sqs_queue.orders"));
    assert!(ids.contains(&"res:qwtm/infra@data.aws_ami.app"));
    assert!(ids.contains(&"res:qwtm/infra@module.vpc"));
    let queue = ex
        .nodes
        .iter()
        .find(|n| n.id == "res:qwtm/infra@aws_sqs_queue.orders")
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
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_instance.app",
        "res:qwtm/infra@data.aws_ami.app"
    )));
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_instance.app",
        "res:qwtm/infra@module.vpc"
    )));
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_lambda_function.fulfill",
        "res:qwtm/infra@aws_iam_role.lambda_role"
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
        "res:qwtm/infra@aws_sns_topic_subscription.alerts",
        "res:qwtm/infra@aws_sqs_queue.orders"
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
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_lambda_function.fulfill"
        )]
    );
    let edge = ex.edges.iter().find(|e| e.label == "TRIGGERS").unwrap();
    assert_eq!(
        edge.props["via"],
        "res:qwtm/infra@aws_lambda_event_source_mapping.orders_to_fulfill"
    );
    assert_eq!(edge.props["registry"], registry::REGISTRY_VERSION);
}

#[test]
fn capability_registry_emits_subscribes() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let subs = edge_pairs(&ex, "SUBSCRIBES");
    assert_eq!(
        subs,
        vec![(
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_sns_topic.alerts"
        )]
    );
}

#[test]
fn capability_registry_routes_nested_lb_listener_action() {
    // AC-0008: the selector refactor preserves refs inside a nested target
    // block for the original ALB registry entry.
    let source = r#"
resource "aws_lb" "public" {}
resource "aws_lb_target_group" "app" {}
resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.public.arn
  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app.arn
  }
}
"#;
    let ex = extract_source(source, "alb.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "ROUTES"),
        vec![(
            "res:qwtm/infra@aws_lb.public",
            "res:qwtm/infra@aws_lb_target_group.app"
        )]
    );
}

#[test]
fn capability_registry_routes_api_gateway_integrations() {
    // AC-0043: API Gateway v1 integration mediates REST API -> target ROUTES.
    let source = r#"
resource "aws_api_gateway_rest_api" "orders" {}
resource "aws_lambda_function" "handler" {}
resource "aws_api_gateway_integration" "orders" {
  rest_api_id = aws_api_gateway_rest_api.orders.id
  uri         = aws_lambda_function.handler.invoke_arn
}
"#;
    let ex = extract_source(source, "api.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "ROUTES"),
        vec![(
            "res:qwtm/infra@aws_api_gateway_rest_api.orders",
            "res:qwtm/infra@aws_lambda_function.handler"
        )]
    );
    let edge = ex.edges.iter().find(|edge| edge.label == "ROUTES").unwrap();
    assert_eq!(
        edge.props["via"],
        "res:qwtm/infra@aws_api_gateway_integration.orders"
    );
    assert_eq!(edge.props["registry"], registry::REGISTRY_VERSION);
}

#[test]
fn capability_registry_triggers_through_lambda_permissions() {
    // AC-0044: a permission's source ARN invokes its Lambda function.
    let source = r#"
resource "aws_cloudwatch_event_rule" "nightly" {}
resource "aws_lambda_function" "worker" {}
resource "aws_lambda_permission" "nightly" {
  source_arn    = aws_cloudwatch_event_rule.nightly.arn
  function_name = aws_lambda_function.worker.function_name
}
"#;
    let ex = extract_source(source, "permission.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "TRIGGERS"),
        vec![(
            "res:qwtm/infra@aws_cloudwatch_event_rule.nightly",
            "res:qwtm/infra@aws_lambda_function.worker"
        )]
    );
}

#[test]
fn capability_registry_triggers_lambda_at_edge_from_nested_cache_behaviors() {
    // AC-0045: both CloudFront cache-behavior forms select nested Lambda ARNs;
    // unrelated nested blocks must not broaden the deterministic match.
    let source = r#"
resource "aws_lambda_function" "viewer_request" {}
resource "aws_lambda_function" "origin_response" {}
resource "aws_lambda_function" "unrelated" {}
resource "aws_cloudfront_distribution" "site" {
  default_cache_behavior {
    lambda_function_association {
      event_type = "viewer-request"
      lambda_arn = aws_lambda_function.viewer_request.qualified_arn
    }
  }
  ordered_cache_behavior {
    lambda_function_association {
      event_type = "origin-response"
      lambda_arn = aws_lambda_function.origin_response.qualified_arn
    }
  }
  origin {
    lambda_arn = aws_lambda_function.unrelated.arn
  }
}
"#;
    let ex = extract_source(source, "cloudfront.tf", &id()).unwrap();
    let triggers = edge_pairs(&ex, "TRIGGERS");
    assert_eq!(triggers.len(), 2);
    assert!(triggers.contains(&(
        "res:qwtm/infra@aws_cloudfront_distribution.site",
        "res:qwtm/infra@aws_lambda_function.viewer_request"
    )));
    assert!(triggers.contains(&(
        "res:qwtm/infra@aws_cloudfront_distribution.site",
        "res:qwtm/infra@aws_lambda_function.origin_response"
    )));
    assert!(!triggers.iter().any(|(_, dst)| dst.contains("unrelated")));
}

#[test]
fn capability_registry_triggers_aws_pipes() {
    // AC-0046: EventBridge Pipes deterministically connect source -> target.
    let source = r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "dispatch" {}
resource "aws_pipes_pipe" "orders" {
  source = aws_sqs_queue.orders.arn
  target = aws_lambda_function.dispatch.arn
}
"#;
    let ex = extract_source(source, "pipes.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "TRIGGERS"),
        vec![(
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_lambda_function.dispatch"
        )]
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
            "res:qwtm/infra@aws_iam_role_policy.fulfill_policy",
            "res:qwtm/infra@aws_sqs_queue.orders"
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
        .find(|n| n.id == "res:qwtm/infra@aws_lambda_event_source_mapping.orders_to_fulfill")
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
