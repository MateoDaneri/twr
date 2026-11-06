/* Synthetic video only. No capture, media-library, audio or user-file reads. */
#import <AVFoundation/AVFoundation.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc != 2 && argc != 3) return 2;
    NSString *codec = argc == 3 ? [NSString stringWithUTF8String:argv[2]] : @"h264";
    if (![codec isEqualToString:@"h264"] && ![codec isEqualToString:@"hevc"]) return 2;
    NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]];
    if ([[NSFileManager defaultManager] fileExistsAtPath:url.path]) return 3;
    NSError *error = nil;
    AVAssetWriter *writer = [[AVAssetWriter alloc] initWithURL:url fileType:AVFileTypeMPEG4 error:&error];
    if (!writer) { fprintf(stderr, "writer: %s\n", error.localizedDescription.UTF8String); return 4; }
    NSDictionary *settings = @{AVVideoCodecKey:([codec isEqualToString:@"hevc"] ? AVVideoCodecTypeHEVC : AVVideoCodecTypeH264),
      AVVideoWidthKey:@320, AVVideoHeightKey:@180,
      AVVideoCompressionPropertiesKey:@{AVVideoAverageBitRateKey:@250000, AVVideoMaxKeyFrameIntervalKey:@12}};
    AVAssetWriterInput *input = [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeVideo outputSettings:settings];
    input.expectsMediaDataInRealTime = NO;
    NSDictionary *attributes = @{(id)kCVPixelBufferPixelFormatTypeKey:@(kCVPixelFormatType_32BGRA),
      (id)kCVPixelBufferWidthKey:@320, (id)kCVPixelBufferHeightKey:@180};
    AVAssetWriterInputPixelBufferAdaptor *adaptor = [AVAssetWriterInputPixelBufferAdaptor assetWriterInputPixelBufferAdaptorWithAssetWriterInput:input sourcePixelBufferAttributes:attributes];
    if (![writer canAddInput:input]) return 5;
    [writer addInput:input];
    if (![writer startWriting]) { fprintf(stderr,"start: %s\n",writer.error.description.UTF8String); return 6; }
    [writer startSessionAtSourceTime:kCMTimeZero];
    CFAbsoluteTime started = CFAbsoluteTimeGetCurrent();
    for (int frame = 0; frame < 24; frame++) {
      while (!input.readyForMoreMediaData) {
        if (writer.status != AVAssetWriterStatusWriting || CFAbsoluteTimeGetCurrent()-started > 20) {
          [writer cancelWriting]; return 7;
        }
        [NSThread sleepForTimeInterval:0.005];
      }
      CVPixelBufferRef pixels = NULL;
      if (CVPixelBufferCreate(kCFAllocatorDefault,320,180,kCVPixelFormatType_32BGRA,(__bridge CFDictionaryRef)attributes,&pixels) != kCVReturnSuccess) { [writer cancelWriting]; return 8; }
      CVPixelBufferLockBaseAddress(pixels,0);
      uint8_t *data = CVPixelBufferGetBaseAddress(pixels);
      size_t stride = CVPixelBufferGetBytesPerRow(pixels);
      memset(data,0,stride*180);
      for (int y=0;y<180;y++) for (int x=0;x<320;x++) {
        uint8_t *p = data+y*stride+x*4;
        BOOL square = x>=frame*11 && x<frame*11+48 && y>=45 && y<95;
        BOOL progress = y>=150 && x<((frame+1)*320/24);
        p[0]=square?35:progress?30:70;
        p[1]=square?55:progress?210:(35+y/4);
        p[2]=square?240:progress?245:20;
        p[3]=255;
      }
      CVPixelBufferUnlockBaseAddress(pixels,0);
      BOOL ok = [adaptor appendPixelBuffer:pixels withPresentationTime:CMTimeMake(frame,12)];
      CVPixelBufferRelease(pixels);
      if (!ok) { fprintf(stderr,"append: %s\n",writer.error.localizedDescription.UTF8String); [writer cancelWriting]; return 9; }
    }
    [input markAsFinished];
    [writer endSessionAtSourceTime:CMTimeMake(24,12)];
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    [writer finishWritingWithCompletionHandler:^{ dispatch_semaphore_signal(done); }];
    if (dispatch_semaphore_wait(done,dispatch_time(DISPATCH_TIME_NOW,20*NSEC_PER_SEC)) != 0) { [writer cancelWriting]; return 10; }
    if (writer.status != AVAssetWriterStatusCompleted) { fprintf(stderr,"finish: %s\n",writer.error.localizedDescription.UTF8String); return 11; }
    printf("24 synthetic %s frames,320x180,12fps,2s\n", codec.UTF8String);
  }
  return 0;
}
