#!/usr/bin/env node
import { App, RemovalPolicy } from 'aws-cdk-lib';
import { DynamoDbStack } from '../stacks/dynamodb-stack.ts';
import { EcsStack } from '../stacks/ecs-stack.ts';
import { LambdaStack } from '../stacks/lambda-stack.ts';
import { NetworkStack } from '../stacks/network-stack.ts';
import { ProviderMockStack } from '../stacks/provider-mock-stack.ts';
import { ReconciliationStack } from '../stacks/reconciliation-stack.ts';
import { S3Stack } from '../stacks/s3-stack.ts';
import { SecretsStack } from '../stacks/secrets-stack.ts';
import { ServiceDiscoveryStack } from '../stacks/service-discovery-stack.ts';

const app = new App();
const anthropicBaseUrl = 'https://api.anthropic.com';
const openAiBaseUrl = 'https://api.openai.com';
const integrationAnthropicBaseUrl = 'http://anthropic-provider-mock.integration.internal:8080';
const integrationOpenAiBaseUrl = 'http://openai-provider-mock.integration.internal:8080';

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

const reconciliationStack = new ReconciliationStack(app, 'InternalAiGatewayReconciliationStack', {
	queueNamePrefix: 'internal-ai-gateway-token-reconciliation',
	removalPolicy: RemovalPolicy.RETAIN,
});

const secretsStack = new SecretsStack(app, 'InternalAiGatewaySecretsStack', {
	removalPolicy: RemovalPolicy.RETAIN,
	secretNamePrefix: 'internal-ai-gateway',
});

new EcsStack(app, 'InternalAiGatewayEcsStack', {
	anthropicApiKeySecret: secretsStack.anthropicApiKeySecret,
	anthropicBaseUrl,
	engineersApiKeyIndexName: dynamoDbStack.engineersApiKeyIndexName,
	engineersTable: dynamoDbStack.engineersTable,
	openAiApiKeySecret: secretsStack.openAiApiKeySecret,
	openAiBaseUrl,
	proxyApiKeyHashSecret: secretsStack.proxyApiKeyHashSecret,
	...(proxyCertificateArn ? { proxyCertificateArn } : {}),
	...(proxyDomainName ? { proxyDomainName } : {}),
	proxyLogGroupName: '/internal-ai-gateway/proxy',
	proxyResourceName: 'internal-ai-gateway-proxy',
	rateLimitTable: dynamoDbStack.rateLimitTable,
	removalPolicy: RemovalPolicy.RETAIN,
	tokenReconciliationQueue: reconciliationStack.tokenReconciliationQueue,
	tokenUsageTable: dynamoDbStack.tokenUsageTable,
	vpc: networkStack.vpc,
});

new S3Stack(app, 'InternalAiGatewayS3Stack');

if (integrationTestsEnabled) {
	const integrationNetworkStack = new NetworkStack(app, 'InternalAiGatewayIntegrationNetworkStack');
	const integrationServiceDiscoveryStack = new ServiceDiscoveryStack(
		app,
		'InternalAiGatewayIntegrationServiceDiscoveryStack',
		{
			vpc: integrationNetworkStack.vpc,
		},
	);
	const integrationDynamoDbStack = new DynamoDbStack(
		app,
		'InternalAiGatewayIntegrationDynamoDbStack',
		{
			removalPolicy: RemovalPolicy.DESTROY,
		},
	);

	const integrationReconciliationStack = new ReconciliationStack(
		app,
		'InternalAiGatewayIntegrationReconciliationStack',
		{
			queueNamePrefix: 'internal-ai-gateway-integration-token-reconciliation',
			removalPolicy: RemovalPolicy.DESTROY,
		},
	);

	const integrationSecretsStack = new SecretsStack(
		app,
		'InternalAiGatewayIntegrationSecretsStack',
		{
			removalPolicy: RemovalPolicy.DESTROY,
			secretNamePrefix: 'internal-ai-gateway/integration',
		},
	);
	const integrationEcsStack = new EcsStack(app, 'InternalAiGatewayIntegrationEcsStack', {
		anthropicApiKeySecret: integrationSecretsStack.anthropicApiKeySecret,
		anthropicBaseUrl: integrationAnthropicBaseUrl,
		engineersApiKeyIndexName: integrationDynamoDbStack.engineersApiKeyIndexName,
		engineersTable: integrationDynamoDbStack.engineersTable,
		openAiApiKeySecret: integrationSecretsStack.openAiApiKeySecret,
		openAiBaseUrl: integrationOpenAiBaseUrl,
		proxyApiKeyHashSecret: integrationSecretsStack.proxyApiKeyHashSecret,
		proxyLogGroupName: '/internal-ai-gateway/integration/proxy',
		proxyResourceName: 'internal-ai-gateway-integration-proxy',
		rateLimitTable: integrationDynamoDbStack.rateLimitTable,
		removalPolicy: RemovalPolicy.DESTROY,
		tokenReconciliationQueue: integrationReconciliationStack.tokenReconciliationQueue,
		tokenUsageTable: integrationDynamoDbStack.tokenUsageTable,
		vpc: integrationNetworkStack.vpc,
	});
	new ProviderMockStack(app, 'InternalAiGatewayIntegrationProviderMockStack', {
		cluster: integrationEcsStack.cluster,
		namespace: integrationServiceDiscoveryStack.namespace,
		proxySecurityGroup: integrationEcsStack.proxyServiceSecurityGroup,
		vpc: integrationNetworkStack.vpc,
	});
}
