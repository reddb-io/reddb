# AWS Lambda + EFS (PLAN.md Phase 10.1)

**Caveat first.** Lambda is a poor fit for RedDB as the primary writer. Lambda has:

- 15-minute max execution time (Lambda will SIGKILL — `RED_BACKUP_ON_SHUTDOWN` won't run).
- Cold-start penalty per invocation that competes with `RED_AUTO_RESTORE`.
- No persistent process between invocations — the writer-lease loop and replica fetcher don't fit the Lambda model.

This deploy targets a narrower use case: **read-only query Lambda** that opens a read replica DB from EFS, serves a single read, and exits. Writes go to a separate ECS / Cloud Run / Fly Machines primary.

## Architecture

```
┌──────────────┐
│   Primary    │  (ECS Fargate, Cloud Run, Fly Machines)
│   (writer)   │  RED_BACKEND=s3, RED_LEASE_REQUIRED=true
└──────┬───────┘
       │ archived to S3
       ▼
   S3 bucket
       │ EFS replication (or scheduled rsync)
       ▼
   EFS volume  ───── mounted at /mnt/reddb on Lambda
                            │
                            ▼
                  ┌──────────────────┐
                  │  Lambda function │  RED_REPLICATION_MODE=replica
                  │   (read query)   │  read_only=true
                  └──────────────────┘
```

## SAM template

Save as `template.yaml`:

```yaml
AWSTemplateFormatVersion: '2010-09-09'
Transform: AWS::Serverless-2016-10-31

Resources:
  RedDBQueryFn:
    Type: AWS::Serverless::Function
    Properties:
      PackageType: Image
      ImageUri: ACCOUNT_ID.dkr.ecr.us-east-1.amazonaws.com/reddb-lambda:latest
      MemorySize: 2048
      Timeout: 60
      Architectures: [x86_64]
      VpcConfig:
        SecurityGroupIds: [!Ref FunctionSG]
        SubnetIds: [!Ref PrivateSubnet1, !Ref PrivateSubnet2]
      FileSystemConfigs:
        - Arn: !GetAtt EfsAccessPoint.Arn
          LocalMountPath: /mnt/reddb
      Environment:
        Variables:
          RED_REPLICATION_MODE: replica
          RED_PATH: /mnt/reddb/data.rdb
          RED_READONLY: "true"
          RED_LOG_FORMAT: json
```

## Limitations

- The replica's WAL apply loop runs only while the Lambda is warm. Reads against this replica may be **arbitrarily stale** — the write side keeps appending to S3 / EFS, but Lambda only catches up when invoked.
- No `red doctor` integration — Lambda doesn't expose `/admin/status` over HTTP. Use CloudWatch Logs + custom metric filters instead.
- For a "real" read replica that follows the primary continuously, prefer ECS Fargate or App Runner.

## When this fits

- Sporadic, read-heavy workloads with no SLA on freshness.
- Cost-sensitive deployments where keeping a replica running is wasteful.
- Edge functions that need point-in-time snapshots of the database.

## When this does NOT fit

- Anything requiring writes.
- Anything requiring sub-second consistency.
- Anything requiring continuous WAL apply (lease + heartbeat).
