#!/usr/bin/env node
import { App } from 'aws-cdk-lib';
import { DynamoDbStack } from '../lib/dynamodb-stack.ts';
import { EcsStack } from '../lib/ecs-stack.ts';
import { LambdaStack } from '../lib/lambda-stack.ts';
import { NetworkStack } from '../lib/network-stack.ts';
import { S3Stack } from '../lib/s3-stack.ts';
import { SecretsStack } from '../lib/secrets-stack.ts';

const app = new App();

const dynamoDbStack = new DynamoDbStack(app, 'InternalAiGatewayDynamoDbStack');
new LambdaStack(app, 'InternalAiGatewayLambdaStack');
const networkStack = new NetworkStack(app, 'InternalAiGatewayNetworkStack');
const secretsStack = new SecretsStack(app, 'InternalAiGatewaySecretsStack');
new EcsStack(app, 'InternalAiGatewayEcsStack', {
	engineersApiKeyIndexName: dynamoDbStack.engineersApiKeyIndexName,
	engineersTable: dynamoDbStack.engineersTable,
	proxyApiKeyHashSecret: secretsStack.proxyApiKeyHashSecret,
	rateLimitTable: dynamoDbStack.rateLimitTable,
	vpc: networkStack.vpc,
});
new S3Stack(app, 'InternalAiGatewayS3Stack');
