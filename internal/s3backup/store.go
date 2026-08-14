package s3backup

import (
	"context"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	awsconfig "github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/credentials"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

type Config struct {
	Bucket          string
	Prefix          string
	Region          string
	Endpoint        string
	AccessKeyID     string
	SecretAccessKey string
	SessionToken    string
}

type object struct {
	key      string
	modified time.Time
}

type backend interface {
	list(context.Context, string, string) ([]object, error)
	put(context.Context, string, string, io.ReadSeeker, int64) error
	get(context.Context, string, string, io.Writer) error
}

type Store struct {
	bucket  string
	prefix  string
	backend backend
}

func New(ctx context.Context, cfg Config) (*Store, error) {
	if strings.TrimSpace(cfg.Bucket) == "" {
		return nil, fmt.Errorf("S3 bucket must not be empty")
	}
	accessKeyID := strings.TrimSpace(cfg.AccessKeyID)
	secretAccessKey := strings.TrimSpace(cfg.SecretAccessKey)
	if (accessKeyID == "") != (secretAccessKey == "") {
		return nil, fmt.Errorf("S3 access key ID and secret access key must be set together")
	}
	loadOptions := []func(*awsconfig.LoadOptions) error{awsconfig.WithRegion(cfg.Region)}
	if accessKeyID != "" {
		loadOptions = append(loadOptions, awsconfig.WithCredentialsProvider(
			credentials.NewStaticCredentialsProvider(accessKeyID, secretAccessKey, strings.TrimSpace(cfg.SessionToken)),
		))
	}
	awsConfig, err := awsconfig.LoadDefaultConfig(ctx, loadOptions...)
	if err != nil {
		return nil, fmt.Errorf("load AWS configuration: %w", err)
	}
	client := s3.NewFromConfig(awsConfig, func(options *s3.Options) {
		if endpoint := strings.TrimSpace(cfg.Endpoint); endpoint != "" {
			options.BaseEndpoint = aws.String(endpoint)
			options.UsePathStyle = true
		}
	})
	return newStore(cfg, &awsBackend{client: client}), nil
}

func newStore(cfg Config, client backend) *Store {
	return &Store{
		bucket:  strings.TrimSpace(cfg.Bucket),
		prefix:  strings.Trim(strings.TrimSpace(cfg.Prefix), "/"),
		backend: client,
	}
}

func (s *Store) Upload(ctx context.Context, filePath string) (string, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return "", fmt.Errorf("open backup for S3 upload: %w", err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", fmt.Errorf("stat backup for S3 upload: %w", err)
	}
	if info.Size() == 0 {
		return "", fmt.Errorf("refuse to upload empty database backup")
	}
	key := path.Join(s.prefix, filepath.Base(filePath))
	if err := s.backend.put(ctx, s.bucket, key, file, info.Size()); err != nil {
		return "", fmt.Errorf("upload S3 backup %q: %w", key, err)
	}
	return key, nil
}

func (s *Store) RestoreIfMissing(ctx context.Context, databasePath string) (bool, string, error) {
	if _, err := os.Stat(databasePath); err == nil {
		return false, "", nil
	} else if !os.IsNotExist(err) {
		return false, "", fmt.Errorf("stat local database: %w", err)
	}

	objects, err := s.backend.list(ctx, s.bucket, s.listPrefix())
	if err != nil {
		return false, "", fmt.Errorf("list S3 backups: %w", err)
	}
	var latest object
	for _, item := range objects {
		if !strings.HasSuffix(item.key, ".db") || item.modified.Before(latest.modified) {
			continue
		}
		latest = item
	}
	if latest.key == "" {
		return false, "", nil
	}

	directory := filepath.Dir(databasePath)
	if err := os.MkdirAll(directory, 0o700); err != nil {
		return false, "", fmt.Errorf("create database directory: %w", err)
	}
	temporary, err := os.CreateTemp(directory, ".mneme-s3-restore-*.db")
	if err != nil {
		return false, "", fmt.Errorf("create temporary restored database: %w", err)
	}
	temporaryPath := temporary.Name()
	defer os.Remove(temporaryPath)
	closed := false
	defer func() {
		if !closed {
			_ = temporary.Close()
		}
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return false, "", fmt.Errorf("secure restored database: %w", err)
	}
	if err := s.backend.get(ctx, s.bucket, latest.key, temporary); err != nil {
		return false, "", fmt.Errorf("download S3 backup %q: %w", latest.key, err)
	}
	info, err := temporary.Stat()
	if err != nil {
		return false, "", fmt.Errorf("stat restored database: %w", err)
	}
	if info.Size() == 0 {
		return false, "", fmt.Errorf("S3 backup %q is empty", latest.key)
	}
	if err := temporary.Sync(); err != nil {
		return false, "", fmt.Errorf("sync restored database: %w", err)
	}
	if err := temporary.Close(); err != nil {
		return false, "", fmt.Errorf("close restored database: %w", err)
	}
	closed = true
	// A hard link installs the restored file atomically and, unlike Rename,
	// refuses to overwrite a local database created after the initial check.
	if err := os.Link(temporaryPath, databasePath); err != nil {
		return false, "", fmt.Errorf("install restored database without overwriting local data: %w", err)
	}
	return true, latest.key, nil
}

func (s *Store) listPrefix() string {
	if s.prefix == "" {
		return ""
	}
	return s.prefix + "/"
}

type awsBackend struct {
	client *s3.Client
}

func (b *awsBackend) list(ctx context.Context, bucket, prefix string) ([]object, error) {
	var objects []object
	paginator := s3.NewListObjectsV2Paginator(b.client, &s3.ListObjectsV2Input{
		Bucket: aws.String(bucket),
		Prefix: aws.String(prefix),
	})
	for paginator.HasMorePages() {
		page, err := paginator.NextPage(ctx)
		if err != nil {
			return nil, err
		}
		for _, item := range page.Contents {
			objects = append(objects, object{key: aws.ToString(item.Key), modified: aws.ToTime(item.LastModified)})
		}
	}
	return objects, nil
}

func (b *awsBackend) put(ctx context.Context, bucket, key string, body io.ReadSeeker, size int64) error {
	_, err := b.client.PutObject(ctx, &s3.PutObjectInput{
		Bucket:        aws.String(bucket),
		Key:           aws.String(key),
		Body:          body,
		ContentLength: aws.Int64(size),
		ContentType:   aws.String("application/x-sqlite3"),
	})
	return err
}

func (b *awsBackend) get(ctx context.Context, bucket, key string, destination io.Writer) error {
	result, err := b.client.GetObject(ctx, &s3.GetObjectInput{
		Bucket: aws.String(bucket),
		Key:    aws.String(key),
	})
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(destination, result.Body)
	closeErr := result.Body.Close()
	if copyErr != nil {
		return copyErr
	}
	return closeErr
}
