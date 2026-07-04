import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { EcsStack } from './ecs-stack.ts';
import { NetworkStack } from './network-stack.ts';

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
						Name: 'MAX_ACTIVE_STREAMS',
						Value: '200',
					},
					{
						Name: 'PORT',
						Value: '8080',
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
					Condition: {
						StringEquals: {
							's3:x-amz-acl': 'bucket-owner-full-control',
						},
					},
					Principal: {
						Service: Match.arrayWith(['delivery.logs.amazonaws.com']),
					},
				}),
				Match.objectLike({
					Action: 's3:GetBucketAcl',
					Principal: {
						Service: Match.arrayWith(['delivery.logs.amazonaws.com']),
					},
				}),
			]),
		},
	});
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
		AlarmDescription: 'Proxy active streams per task are close to the hard limit.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 2,
		MetricName: 'ActiveStreams',
		Namespace: 'InternalAiGateway/Proxy',
		Threshold: 180,
		TreatMissingData: 'notBreaching',
	});
	template.hasResourceProperties('AWS::CloudWatch::Alarm', {
		AlarmDescription: 'At least one proxy ALB target is unhealthy.',
		ComparisonOperator: 'GreaterThanOrEqualToThreshold',
		EvaluationPeriods: 2,
		MetricName: 'UnHealthyHostCount',
		Namespace: 'AWS/ApplicationELB',
		Threshold: 1,
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
	const networkStack = new NetworkStack(app, 'TestNetworkStack');
	const ecsStack = new EcsStack(app, 'TestEcsStack', {
		vpc: networkStack.vpc,
	});

	return Template.fromStack(ecsStack);
}
