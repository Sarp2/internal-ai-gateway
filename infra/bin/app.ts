#!/usr/bin/env node
import { App } from 'aws-cdk-lib';
import { DynamoDbStack } from '../lib/dynamodb-stack.ts';
import { LambdaStack } from '../lib/lambda-stack.ts';
import { S3Stack } from '../lib/s3-stack.ts';
import { SecretsStack } from '../lib/secrets-stack.ts';

const app = new App();

new DynamoDbStack(app, 'InternalAiGatewayDynamoDbStack');
new LambdaStack(app, 'InternalAiGatewayLambdaStack');
new S3Stack(app, 'InternalAiGatewayS3Stack');
new SecretsStack(app, 'InternalAiGatewaySecretsStack');
