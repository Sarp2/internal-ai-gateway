#!/usr/bin/env node
import { App } from 'aws-cdk-lib';
import { InternalAiGatewayStack } from '../lib/app-stack.ts';

const app = new App();

new InternalAiGatewayStack(app, 'InternalAiGatewayStack');
