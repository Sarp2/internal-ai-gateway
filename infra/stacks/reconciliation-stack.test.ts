import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App, RemovalPolicy } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { ReconciliationStack } from './reconciliation-stack.ts';

test('defines durable token reconciliation queues', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::SQS::Queue', 2);
	template.hasResourceProperties('AWS::SQS::Queue', {
		QueueName: 'internal-ai-gateway-token-reconciliation',
		MessageRetentionPeriod: 86_400,
		ReceiveMessageWaitTimeSeconds: 20,
		RedrivePolicy: {
			deadLetterTargetArn: Match.anyValue(),
			maxReceiveCount: 5,
		},
		SqsManagedSseEnabled: true,
		VisibilityTimeout: 300,
	});
	template.hasResourceProperties('AWS::SQS::Queue', {
		QueueName: 'internal-ai-gateway-token-reconciliation-dlq',
		MessageRetentionPeriod: 1_209_600,
		SqsManagedSseEnabled: true,
	});
});

test('retains token reconciliation queues when the stack is deleted', () => {
	const template = synthesizeTemplate();
	const queues = template.findResources('AWS::SQS::Queue');

	for (const queue of Object.values(queues)) {
		strictEqual(queue.DeletionPolicy, 'Retain');
		strictEqual(queue.UpdateReplacePolicy, 'Retain');
	}
});

test('denies non-SSL access to token reconciliation queues', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::SQS::QueuePolicy', 2);
	template.hasResourceProperties('AWS::SQS::QueuePolicy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: 'sqs:*',
					Condition: {
						Bool: {
							'aws:SecureTransport': 'false',
						},
					},
					Effect: 'Deny',
				}),
			]),
		},
	});
});

test('alarms when token reconciliation jobs reach the dead-letter queue', () => {
	const template = synthesizeTemplate();
	const deadLetterQueueLogicalId = Object.entries(template.findResources('AWS::SQS::Queue')).find(
		([, queue]) => queue.Properties.QueueName === 'internal-ai-gateway-token-reconciliation-dlq',
	)?.[0];

	if (!deadLetterQueueLogicalId) {
		throw new Error('token reconciliation dead-letter queue must exist');
	}

	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Token reconciliation jobs reached the dead-letter queue.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		Dimensions: [
			{
				Name: 'QueueName',
				Value: {
					'Fn::GetAtt': [deadLetterQueueLogicalId, 'QueueName'],
				},
			},
		],
		EvaluationPeriods: 1,
		MetricName: 'ApproximateNumberOfMessagesVisible',
		Namespace: 'AWS/SQS',
		Threshold: 1,
		TreatMissingData: 'notBreaching',
	});
});

test('defines isolated integration queues with production behavior', () => {
	const template = synthesizeIntegrationTemplate();

	template.resourceCountIs('AWS::SQS::Queue', 2);
	template.hasResourceProperties('AWS::SQS::Queue', {
		QueueName: 'internal-ai-gateway-integration-token-reconciliation',
		MessageRetentionPeriod: 86_400,
		ReceiveMessageWaitTimeSeconds: 20,
		RedrivePolicy: {
			deadLetterTargetArn: Match.anyValue(),
			maxReceiveCount: 5,
		},
		SqsManagedSseEnabled: true,
		VisibilityTimeout: 300,
	});
	template.hasResourceProperties('AWS::SQS::Queue', {
		QueueName: 'internal-ai-gateway-integration-token-reconciliation-dlq',
		MessageRetentionPeriod: 1_209_600,
		SqsManagedSseEnabled: true,
	});
});

test('destroys integration reconciliation queues with their stack', () => {
	const queues = synthesizeIntegrationTemplate().findResources('AWS::SQS::Queue');

	for (const queue of Object.values(queues)) {
		strictEqual(queue.DeletionPolicy, 'Delete');
		strictEqual(queue.UpdateReplacePolicy, 'Delete');
	}
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new ReconciliationStack(app, 'TestReconciliationStack');

	return Template.fromStack(stack);
}

function synthesizeIntegrationTemplate(): Template {
	const app = new App();
	const stack = new ReconciliationStack(app, 'TestIntegrationReconciliationStack', {
		queueNamePrefix: 'internal-ai-gateway-integration-token-reconciliation',
		removalPolicy: RemovalPolicy.DESTROY,
	});

	return Template.fromStack(stack);
}
