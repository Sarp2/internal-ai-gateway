import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { DynamoDbStack } from './dynamodb-stack.ts';

test('defines the DynamoDB tables for the gateway', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::DynamoDB::Table', 3);

	const tables = template.findResources('AWS::DynamoDB::Table');

	for (const table of Object.values(tables)) {
		strictEqual(table.DeletionPolicy, 'Retain');
		strictEqual(table.UpdateReplacePolicy, 'Retain');
	}
});

test('defines engineers table with api_key_hash lookup index', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [{ AttributeName: 'user_id', KeyType: 'HASH' }],
		AttributeDefinitions: Match.arrayWith([
			{ AttributeName: 'user_id', AttributeType: 'S' },
			{ AttributeName: 'api_key_hash', AttributeType: 'S' },
		]),
		BillingMode: 'PAY_PER_REQUEST',
		GlobalSecondaryIndexes: [
			{
				IndexName: 'ApiKeyIndex',
				KeySchema: [{ AttributeName: 'api_key_hash', KeyType: 'HASH' }],
				Projection: { ProjectionType: 'ALL' },
			},
		],
	});
});

test('defines messages table ordered by message_id per user', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [
			{ AttributeName: 'user_id', KeyType: 'HASH' },
			{ AttributeName: 'message_id', KeyType: 'RANGE' },
		],
		AttributeDefinitions: Match.arrayWith([
			{ AttributeName: 'user_id', AttributeType: 'S' },
			{ AttributeName: 'message_id', AttributeType: 'S' },
		]),
		BillingMode: 'PAY_PER_REQUEST',
	});
});

test('defines rate limit table with TTL for sliding window cleanup', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [
			{ AttributeName: 'user_id', KeyType: 'HASH' },
			{ AttributeName: 'request_ts', KeyType: 'RANGE' },
		],
		AttributeDefinitions: Match.arrayWith([
			{ AttributeName: 'user_id', AttributeType: 'S' },
			{ AttributeName: 'request_ts', AttributeType: 'N' },
		]),
		BillingMode: 'PAY_PER_REQUEST',
		TimeToLiveSpecification: {
			AttributeName: 'ttl',
			Enabled: true,
		},
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new DynamoDbStack(app, 'TestDynamoDbStack');

	return Template.fromStack(stack);
}
