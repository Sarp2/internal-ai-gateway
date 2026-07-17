#!/usr/bin/env node
import { App, RemovalPolicy } from 'aws-cdk-lib';
import { DynamoDbStack } from '../stacks/dynamodb-stack.ts';
import { EcsStack } from '../stacks/ecs-stack.ts';
import { LambdaStack } from '../stacks/lambda-stack.ts';
import { NetworkStack } from '../stacks/network-stack.ts';
import { ReconciliationStack } from '../stacks/reconciliation-stack.ts';
import { S3Stack } from '../stacks/s3-stack.ts';
import { SecretsStack } from '../stacks/secrets-stack.ts';

const app = new App();
const proxyCertificateArn = app.node.tryGetContext('proxyCertificateArn') as string | undefined;
const proxyDomainName = app.node.tryGetContext('proxyDomainName') as string | undefined;
const integrationTestsContext = app.node.tryGetContext('integrationTests') as unknown;
const integrationTestsEnabled =
	integrationTestsContext === true || integrationTestsContext === 'true';

const dynamoDbStack = new DynamoDbStack(app, 'InternalAiGatewayDynamoDbStack', {
	removalPolicy: RemovalPolicy.RETAIN,
});
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

if (integrationTestsEnabled) {
	new DynamoDbStack(app, 'InternalAiGatewayIntegrationDynamoDbStack', {
		removalPolicy: RemovalPolicy.DESTROY,
	});
}
