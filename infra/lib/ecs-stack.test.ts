import assert from 'node:assert/strict';
import { test } from 'node:test';
import { App, Stack } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { Secret } from 'aws-cdk-lib/aws-secretsmanager';
import { DynamoDbStack } from './dynamodb-stack.ts';
import { EcsStack } from './ecs-stack.ts';
import { NetworkStack } from './network-stack.ts';

type SynthesizedResource = {
	Type?: string;
	DependsOn?: string | string[];
};

test('defines an ECS cluster for proxy workloads', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::ECS::Cluster', 1);
	template.hasResourceProperties('AWS::ECS::Cluster', {
		ClusterName: 'internal-ai-gateway-proxy',
	});
});

test('enables Fargate capacity providers for the cluster', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::ClusterCapacityProviderAssociations', {
		CapacityProviders: ['FARGATE', 'FARGATE_SPOT'],
	});
});

test('enables enhanced container insights for observability', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::Cluster', {
		ClusterSettings: [
			{
				Name: 'containerInsights',
				Value: 'enhanced',
			},
		],
	});
});

test('defines a Fargate task definition for the proxy container', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		Family: 'internal-ai-gateway-proxy',
		Cpu: '512',
		Memory: '1024',
		NetworkMode: 'awsvpc',
		RequiresCompatibilities: ['FARGATE'],
	});
});

test('defines the proxy container port and runtime environment', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		ContainerDefinitions: [
			{
				Name: 'proxy',
				Essential: true,
				Environment: [
					{
						Name: 'ACTIVE_STREAM_METRIC_INTERVAL_SECONDS',
						Value: '15',
					},
					{
						Name: 'ENGINEERS_API_KEY_INDEX_NAME',
						Value: 'ApiKeyIndex',
					},
					{
						Name: 'ENGINEERS_TABLE_NAME',
						Value: Match.anyValue(),
					},
					{
						Name: 'MAX_ACTIVE_STREAMS',
						Value: '200',
					},
					{
						Name: 'PORT',
						Value: '8080',
					},
					{
						Name: 'PROXY_API_KEY_HASH_SECRET_ARN',
						Value: Match.anyValue(),
					},
					{
						Name: 'RATE_LIMIT_REQUESTS_PER_WINDOW',
						Value: '120',
					},
					{
						Name: 'RATE_LIMIT_TABLE_NAME',
						Value: Match.anyValue(),
					},
					{
						Name: 'RATE_LIMIT_WINDOW_SECONDS',
						Value: '60',
					},
					{
						Name: 'RUST_LOG',
						Value: 'info',
					},
				],
				PortMappings: [
					{
						ContainerPort: 8080,
					},
				],
			},
		],
	});
});

test('defines CloudWatch logs for the proxy container', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::Logs::LogGroup', {
		LogGroupName: '/internal-ai-gateway/proxy',
		RetentionInDays: 30,
	});
});

test('defines an encrypted access log bucket for the proxy load balancer', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::S3::Bucket', {
		BucketEncryption: {
			ServerSideEncryptionConfiguration: [
				{
					ServerSideEncryptionByDefault: {
						SSEAlgorithm: 'AES256',
					},
				},
			],
		},
		PublicAccessBlockConfiguration: {
			BlockPublicAcls: true,
			BlockPublicPolicy: true,
			IgnorePublicAcls: true,
			RestrictPublicBuckets: true,
		},
	});
});

test('allows load balancer log delivery to write access logs', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::S3::BucketPolicy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: 's3:PutObject',
					Condition: Match.absent(),
					Principal: {
						Service: 'logdelivery.elasticloadbalancing.amazonaws.com',
					},
				}),
				Match.objectLike({
					Action: 's3:GetBucketAcl',
					Principal: {
						Service: 'logdelivery.elasticloadbalancing.amazonaws.com',
					},
				}),
			]),
		},
	});
});

test('creates the access log bucket policy before enabling load balancer logs', () => {
	const template = synthesizeTemplate();
	const templateJson = template.toJSON();
	const resources = Object.values(templateJson.Resources) as SynthesizedResource[];
	const loadBalancer = resources.find(
		(resource) => resource.Type === 'AWS::ElasticLoadBalancingV2::LoadBalancer',
	);

	assert.ok(loadBalancer);

	const dependsOn = loadBalancer.DependsOn;
	const dependencies = Array.isArray(dependsOn) ? dependsOn : [dependsOn];

	assert.ok(
		dependencies.some(
			(dependency) =>
				typeof dependency === 'string' && dependency.startsWith('ProxyAccessLogBucketPolicy'),
		),
	);
});

test('defines a private Fargate service for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::Service', {
		ServiceName: 'internal-ai-gateway-proxy',
		DesiredCount: 3,
		EnableECSManagedTags: true,
		DeploymentConfiguration: {
			DeploymentCircuitBreaker: {
				Enable: true,
				Rollback: true,
			},
			MaximumPercent: 200,
			MinimumHealthyPercent: 100,
		},
		NetworkConfiguration: {
			AwsvpcConfiguration: {
				AssignPublicIp: 'DISABLED',
			},
		},
	});
});

test('defines a public application load balancer for proxy traffic', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ElasticLoadBalancingV2::LoadBalancer', {
		LoadBalancerAttributes: Match.arrayWith([
			{
				Key: 'idle_timeout.timeout_seconds',
				Value: '300',
			},
			{
				Key: 'access_logs.s3.enabled',
				Value: 'true',
			},
			{
				Key: 'access_logs.s3.prefix',
				Value: 'alb',
			},
		]),
		Scheme: 'internet-facing',
		Type: 'application',
	});
});

test('defines an HTTP listener for the proxy load balancer', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ElasticLoadBalancingV2::Listener', {
		Port: 80,
		Protocol: 'HTTP',
	});
});

test('routes load balancer traffic to the proxy container port', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ElasticLoadBalancingV2::TargetGroup', {
		Port: 8080,
		Protocol: 'HTTP',
		TargetType: 'ip',
	});
});

test('defines the proxy target group health check', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ElasticLoadBalancingV2::TargetGroup', {
		HealthCheckPath: '/health',
		HealthCheckIntervalSeconds: 10,
		HealthCheckTimeoutSeconds: 5,
		HealthyThresholdCount: 2,
		UnhealthyThresholdCount: 3,
		Matcher: {
			HttpCode: '200',
		},
	});
});

test('defines target draining for long-running proxy streams', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ElasticLoadBalancingV2::TargetGroup', {
		TargetGroupAttributes: Match.arrayWith([
			{
				Key: 'deregistration_delay.timeout_seconds',
				Value: '300',
			},
		]),
	});
});

test('allows public HTTP traffic into the load balancer', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::EC2::SecurityGroup', {
		GroupDescription: 'Allows public HTTP traffic to reach the proxy load balancer.',
		SecurityGroupIngress: [
			{
				CidrIp: '0.0.0.0/0',
				FromPort: 80,
				IpProtocol: 'tcp',
				ToPort: 80,
			},
		],
	});
});

test('allows load balancer traffic into proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::EC2::SecurityGroupIngress', {
		FromPort: 8080,
		IpProtocol: 'tcp',
		ToPort: 8080,
	});
});

test('defines autoscaling limits for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalableTarget', {
		MaxCapacity: 30,
		MinCapacity: 3,
		ScalableDimension: 'ecs:service:DesiredCount',
		ServiceNamespace: 'ecs',
	});
});

test('defines CPU and memory target tracking scaling policies', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			PredefinedMetricSpecification: {
				PredefinedMetricType: 'ECSServiceAverageCPUUtilization',
			},
			TargetValue: 60,
		},
	});

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			PredefinedMetricSpecification: {
				PredefinedMetricType: 'ECSServiceAverageMemoryUtilization',
			},
			TargetValue: 60,
		},
	});
});

test('defines request-count target tracking for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			PredefinedMetricSpecification: {
				PredefinedMetricType: 'ALBRequestCountPerTarget',
			},
			ScaleInCooldown: 120,
			ScaleOutCooldown: 60,
			TargetValue: 1000,
		},
	});
});

test('allows proxy tasks to publish active stream metrics', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::IAM::Policy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: 'cloudwatch:PutMetricData',
					Condition: {
						StringEquals: {
							'cloudwatch:namespace': 'InternalAiGateway/Proxy',
						},
					},
					Resource: '*',
				}),
			]),
		},
	});
});

test('allows proxy tasks to read the api key hash secret', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::IAM::Policy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: ['secretsmanager:GetSecretValue', 'secretsmanager:DescribeSecret'],
					Effect: 'Allow',
					Resource: Match.anyValue(),
				}),
			]),
		},
	});
});

test('allows proxy tasks to query engineers by api key hash', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::IAM::Policy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: 'dynamodb:Query',
					Effect: 'Allow',
					Resource: Match.anyValue(),
				}),
			]),
		},
	});
});

test('allows proxy tasks to update rate limit counters', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::IAM::Policy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Action: 'dynamodb:UpdateItem',
					Effect: 'Allow',
					Resource: Match.anyValue(),
				}),
			]),
		},
	});
});

test('defines active-stream target tracking for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			CustomizedMetricSpecification: {
				Dimensions: [
					{
						Name: 'ServiceName',
						Value: 'internal-ai-gateway-proxy',
					},
				],
				MetricName: 'ActiveStreams',
				Namespace: 'InternalAiGateway/Proxy',
				Statistic: 'Average',
				Unit: 'Count',
			},
			ScaleInCooldown: 180,
			ScaleOutCooldown: 30,
			TargetValue: 150,
		},
	});
});

test('defines proxy CloudWatch alarms', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::CloudWatch::Alarm', 6);
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Proxy ECS service CPU utilization is high.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 3,
		MetricName: 'CPUUtilization',
		Namespace: 'AWS/ECS',
		Threshold: 80,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Proxy ECS service memory utilization is high.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 3,
		MetricName: 'MemoryUtilization',
		Namespace: 'AWS/ECS',
		Threshold: 80,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Proxy active streams per task are close to the hard limit.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 2,
		MetricName: 'ActiveStreams',
		Namespace: 'InternalAiGateway/Proxy',
		Statistic: 'Maximum',
		Threshold: 180,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Proxy targets are returning elevated 5xx responses.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 2,
		MetricName: 'HTTPCode_Target_5XX_Count',
		Namespace: 'AWS/ApplicationELB',
		Threshold: 10,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'At least one proxy ALB target is unhealthy.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 2,
		MetricName: 'UnHealthyHostCount',
		Namespace: 'AWS/ApplicationELB',
		Statistic: 'Maximum',
		Threshold: 1,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'Proxy target response time is elevated.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 3,
		MetricName: 'TargetResponseTime',
		Namespace: 'AWS/ApplicationELB',
		Threshold: 5,
		TreatMissingData: 'notBreaching',
	});
});

test('defines a proxy CloudWatch dashboard', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::CloudWatch::Dashboard', {
		DashboardName: 'internal-ai-gateway-proxy',
	});
});

test('defines deploy outputs for proxy endpoints and access logs', () => {
	const template = synthesizeTemplate();

	template.hasOutput('ProxyLoadBalancerDnsName', {
		Description: 'Public DNS name for the proxy Application Load Balancer.',
	});
	template.hasOutput('ProxyHealthUrl', {
		Description: 'Health check URL for the proxy service.',
	});
	template.hasOutput('ProxyAccessLogBucketName', {
		Description: 'S3 bucket that stores proxy ALB access logs.',
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const dynamoDbStack = new DynamoDbStack(app, 'TestDynamoDbStack');
	const networkStack = new NetworkStack(app, 'TestNetworkStack');
	const secretsStack = new Stack(app, 'TestSecretsStack');
	const proxyApiKeyHashSecret = new Secret(secretsStack, 'ProxyApiKeyHashSecret');
	const ecsStack = new EcsStack(app, 'TestEcsStack', {
		engineersApiKeyIndexName: dynamoDbStack.engineersApiKeyIndexName,
		engineersTable: dynamoDbStack.engineersTable,
		proxyApiKeyHashSecret,
		rateLimitTable: dynamoDbStack.rateLimitTable,
		vpc: networkStack.vpc,
	});

	return Template.fromStack(ecsStack);
}
