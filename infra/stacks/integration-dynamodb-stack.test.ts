import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { IntegrationDynamoDbStack } from './integration-dynamodb-stack.ts';

test('defines isolated integration tables with production schemas', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::DynamoDB::Table', 4);
	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [{ AttributeName: 'user_id', KeyType: 'HASH' }],
		GlobalSecondaryIndexes: [
			{
				IndexName: 'ApiKeyIndex',
				KeySchema: [{ AttributeName: 'api_key_hash', KeyType: 'HASH' }],
				Projection: { ProjectionType: 'ALL' },
			},
		],
	});
	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [
			{ AttributeName: 'user_id', KeyType: 'HASH' },
			{ AttributeName: 'request_ts', KeyType: 'RANGE' },
		],
		TimeToLiveSpecification: {
			AttributeName: 'ttl',
			Enabled: true,
		},
	});
	template.hasResourceProperties('AWS::DynamoDB::Table', {
		KeySchema: [
			{ AttributeName: 'user_id', KeyType: 'HASH' },
			{ AttributeName: 'usage_window', KeyType: 'RANGE' },
		],
		TimeToLiveSpecification: Match.objectLike({ Enabled: true }),
	});
});

test('destroys integration tables with their stack', () => {
	const tables = synthesizeTemplate().findResources('AWS::DynamoDB::Table');

	for (const table of Object.values(tables)) {
		strictEqual(table.DeletionPolicy, 'Delete');
		strictEqual(table.UpdateReplacePolicy, 'Delete');
	}
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new IntegrationDynamoDbStack(app, 'TestIntegrationDynamoDbStack');

	return Template.fromStack(stack);
}
