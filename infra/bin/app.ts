#!/usr/bin/env node
import { App } from 'aws-cdk-lib';
import { DynamoDbStack } from '../lib/dynamodb-stack.ts';
import { EcsStack } from '../lib/ecs-stack.ts';
import { LambdaStack } from '../lib/lambda-stack.ts';
import { NetworkStack } from '../lib/network-stack.ts';
import { ReconciliationStack } from '../lib/reconciliation-stack.ts';
import { S3Stack } from '../lib/s3-stack.ts';
import { SecretsStack } from '../lib/secrets-stack.ts';

const app = new App();
const proxyCertificateArn = app.node.tryGetContext('proxyCertificateArn') as string | undefined;
const proxyDomainName = app.node.tryGetContext('proxyDomainName') as string | undefined;

const dynamoDbStack = new DynamoDbStack(app, 'InternalAiGatewayDynamoDbStack');
new LambdaStack(app, 'InternalAiGatewayLambdaStack');
const networkStack = new NetworkStack(app, 'InternalAiGatewayNetworkStack');
const reconciliationStack = new ReconciliationStack(app, 'InternalAiGatewayReconciliationStack');
const secretsStack = new SecretsStack(app, 'InternalAiGatewaySecretsStack');
new EcsStack(app, 'InternalAiGatewayEcsStack', {
	anthropicApiKeySecret: secretsStack.anthropicApiKeySecret,
	engineersApiKeyIndexName: dynamoDbStack.engineersApiKeyIndexName,
	engineersTable: dynamoDbStack.engineersTable,
	openAiApiKeySecret: secretsStack.openAiApiKeySecret,
	proxyApiKeyHashSecret: secretsStack.proxyApiKeyHashSecret,
	...(proxyCertificateArn ? { proxyCertificateArn } : {}),
	...(proxyDomainName ? { proxyDomainName } : {}),
	rateLimitTable: dynamoDbStack.rateLimitTable,
	tokenReconciliationQueue: reconciliationStack.tokenReconciliationQueue,
	tokenUsageTable: dynamoDbStack.tokenUsageTable,
	vpc: networkStack.vpc,
});
new S3Stack(app, 'InternalAiGatewayS3Stack');
