import { CfnOutput, Duration, RemovalPolicy, Stack, type StackProps } from 'aws-cdk-lib';
import { Alarm, ComparisonOperator, TreatMissingData } from 'aws-cdk-lib/aws-cloudwatch';
import { Queue, QueueEncryption } from 'aws-cdk-lib/aws-sqs';
import type { Construct } from 'constructs';

const defaultQueueNamePrefix = 'internal-ai-gateway-token-reconciliation';

type ReconciliationStackProps = StackProps & {
	queueNamePrefix?: string;
	removalPolicy?: RemovalPolicy;
};

export class ReconciliationStack extends Stack {
	public readonly tokenReconciliationDeadLetterQueue: Queue;
	public readonly tokenReconciliationQueue: Queue;

	public constructor(scope: Construct, id: string, props?: ReconciliationStackProps) {
		super(scope, id, props);
		const queueNamePrefix = props?.queueNamePrefix ?? defaultQueueNamePrefix;
		const removalPolicy = props?.removalPolicy ?? RemovalPolicy.RETAIN;

		this.tokenReconciliationDeadLetterQueue = new Queue(
			this,
			'TokenReconciliationDeadLetterQueue',
			{
				queueName: `${queueNamePrefix}-dlq`,
				encryption: QueueEncryption.SQS_MANAGED,
				enforceSSL: true,
				removalPolicy,
				retentionPeriod: Duration.days(14),
			},
		);

		this.tokenReconciliationQueue = new Queue(this, 'TokenReconciliationQueue', {
			queueName: queueNamePrefix,
			deadLetterQueue: {
				maxReceiveCount: 5,
				queue: this.tokenReconciliationDeadLetterQueue,
			},
			encryption: QueueEncryption.SQS_MANAGED,
			enforceSSL: true,
			removalPolicy,
			receiveMessageWaitTime: Duration.seconds(20),
			retentionPeriod: Duration.days(1),
			visibilityTimeout: Duration.minutes(5),
		});

		new Alarm(this, 'TokenReconciliationDeadLetterAlarm', {
			alarmDescription: 'Token reconciliation jobs reached the dead-letter queue.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 1,
			metric: this.tokenReconciliationDeadLetterQueue.metricApproximateNumberOfMessagesVisible({
				period: Duration.minutes(1),
			}),
			threshold: 1,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new CfnOutput(this, 'TokenReconciliationQueueUrl', {
			description: 'SQS queue URL for durable token reconciliation jobs.',
			value: this.tokenReconciliationQueue.queueUrl,
		});

		new CfnOutput(this, 'TokenReconciliationDeadLetterQueueUrl', {
			description: 'SQS dead-letter queue URL for failed token reconciliation jobs.',
			value: this.tokenReconciliationDeadLetterQueue.queueUrl,
		});
	}
}
