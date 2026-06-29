#!/usr/bin/env node
import { App } from 'aws-cdk-lib';
import { DynamoDbStack } from '../lib/dynamodb-stack.ts';

const app = new App();

new DynamoDbStack(app, 'InternalAiGatewayDynamoDbStack');
