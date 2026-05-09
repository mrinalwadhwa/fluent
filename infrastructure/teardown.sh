#!/usr/bin/env bash
# teardown.sh — Remove all factory Fargate infrastructure from AWS.
#
# Usage: ./infrastructure/teardown.sh [--region REGION]
#
# Empties S3 bucket and ECR repos (CloudFormation can't delete non-empty
# ones), then deletes the stack. Also removes the local fargate.env config.

set -euo pipefail

FACTORY_CONFIG="${FACTORY_CONFIG:-$HOME/.config/factory}"
REGION="${AWS_DEFAULT_REGION:-us-west-1}"
STACK_NAME="factory"

while [ $# -gt 0 ]; do
  case "$1" in
    --region) REGION="$2"; shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

die() { printf 'teardown: %s\n' "$1" >&2; exit 1; }

command -v aws >/dev/null 2>&1 || die "aws CLI not found"

# Check stack exists
STACK_STATUS="$(aws cloudformation describe-stacks \
  --region "$REGION" \
  --stack-name "$STACK_NAME" \
  --query 'Stacks[0].StackStatus' \
  --output text 2>/dev/null || echo "NONE")"

if [ "$STACK_STATUS" = "NONE" ]; then
  printf 'Stack "%s" does not exist in %s. Nothing to tear down.\n' "$STACK_NAME" "$REGION"
  exit 0
fi

printf 'Tearing down stack "%s" in %s (status: %s)\n' "$STACK_NAME" "$REGION" "$STACK_STATUS"

# -------------------------------------------------------------------------
# Stop any running ECS tasks
# -------------------------------------------------------------------------

get_output() {
  aws cloudformation describe-stacks \
    --region "$REGION" \
    --stack-name "$STACK_NAME" \
    --query "Stacks[0].Outputs[?OutputKey=='${1}'].OutputValue" \
    --output text 2>/dev/null || true
}

CLUSTER_ARN="$(get_output ClusterArn)"

if [ -n "$CLUSTER_ARN" ] && [ "$CLUSTER_ARN" != "None" ]; then
  printf '\nStopping running tasks...\n'
  TASK_ARNS="$(aws ecs list-tasks \
    --region "$REGION" \
    --cluster "$CLUSTER_ARN" \
    --query 'taskArns[]' \
    --output text 2>/dev/null || true)"

  if [ -n "$TASK_ARNS" ] && [ "$TASK_ARNS" != "None" ]; then
    for arn in $TASK_ARNS; do
      printf '  Stopping %s\n' "$arn"
      aws ecs stop-task \
        --region "$REGION" \
        --cluster "$CLUSTER_ARN" \
        --task "$arn" > /dev/null 2>&1 || true
    done
    printf '  Waiting for tasks to stop...\n'
    sleep 10
  else
    printf '  No running tasks.\n'
  fi
fi

# -------------------------------------------------------------------------
# Empty S3 bucket
# -------------------------------------------------------------------------

S3_BUCKET="$(get_output WorkspaceBucketName)"

if [ -n "$S3_BUCKET" ] && [ "$S3_BUCKET" != "None" ]; then
  printf '\nEmptying S3 bucket %s...\n' "$S3_BUCKET"
  aws s3 rm "s3://${S3_BUCKET}" --recursive --region "$REGION" 2>/dev/null || true
  printf '  Bucket emptied.\n'
fi

# -------------------------------------------------------------------------
# Delete ECR images
# -------------------------------------------------------------------------

delete_ecr_images() {
  REPO_NAME="$1"
  IMAGES="$(aws ecr list-images \
    --region "$REGION" \
    --repository-name "$REPO_NAME" \
    --query 'imageIds[*]' \
    --output json 2>/dev/null || echo "[]")"

  if [ "$IMAGES" != "[]" ] && [ -n "$IMAGES" ]; then
    printf '  Deleting images from %s...\n' "$REPO_NAME"
    aws ecr batch-delete-image \
      --region "$REGION" \
      --repository-name "$REPO_NAME" \
      --image-ids "$IMAGES" > /dev/null 2>&1 || true
  fi
}

printf '\nCleaning ECR repository...\n'
delete_ecr_images "factory/run" 2>/dev/null || true

# -------------------------------------------------------------------------
# Delete CloudFormation stack
# -------------------------------------------------------------------------

printf '\nDeleting CloudFormation stack...\n'
aws cloudformation delete-stack \
  --region "$REGION" \
  --stack-name "$STACK_NAME"

printf '  Waiting for stack deletion...\n'
aws cloudformation wait stack-delete-complete \
  --region "$REGION" \
  --stack-name "$STACK_NAME" 2>/dev/null || true

printf '  Stack deleted.\n'

# -------------------------------------------------------------------------
# Remove local config
# -------------------------------------------------------------------------

if [ -f "${FACTORY_CONFIG}/fargate.env" ]; then
  rm -f "${FACTORY_CONFIG}/fargate.env"
  printf '\n  Removed %s/fargate.env\n' "$FACTORY_CONFIG"
fi

printf '\nTeardown complete.\n'
