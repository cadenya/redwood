# frozen_string_literal: true

# Standard Webhooks verification (https://www.standardwebhooks.com).
# HMAC-SHA256 over `{id}.{timestamp}.{payload}` keyed by the base64-decoded
# secret (`whsec_` prefix optional); `webhook-signature` may carry several
# space-separated `v1,<base64>` candidates.

require "base64"
require "openssl"

module RedwoodModule
  class WebhookVerificationError < StandardError; end

  module Webhooks
    module_function

    # Raises WebhookVerificationError unless the payload is authentic.
    def verify(secret, payload, headers, tolerance_seconds: 300)
      raise WebhookVerificationError, "webhook secret must be a String" unless secret.is_a?(String)
      normalized = headers.to_h { |k, v| [k.to_s.downcase, v.is_a?(Array) ? v.first : v] }
      msg_id = normalized["webhook-id"]
      timestamp = normalized["webhook-timestamp"]
      signatures = normalized["webhook-signature"]
      if msg_id.to_s.empty? || timestamp.to_s.empty? || signatures.to_s.empty?
        raise WebhookVerificationError, "missing webhook-id, webhook-timestamp, or webhook-signature header"
      end

      # Tolerance must be finite and non-negative: an infinite tolerance
      # would silently disable replay protection.
      unless tolerance_seconds.is_a?(Numeric) && tolerance_seconds.finite? && tolerance_seconds >= 0
        raise WebhookVerificationError, "tolerance_seconds must be a finite non-negative number"
      end

      # The spec calls webhook-timestamp an integer Unix timestamp. Integer()
      # accepts "+1", whitespace, and underscores; require the wire grammar.
      unless /\A[0-9]+\z/.match?(timestamp.to_s)
        raise WebhookVerificationError, "webhook-timestamp is not an integer"
      end
      sent = Integer(timestamp)
      raise WebhookVerificationError, "webhook-timestamp is out of range" if sent > 2**63
      if (Time.now.to_f - sent).abs > tolerance_seconds
        raise WebhookVerificationError, "webhook-timestamp outside tolerance"
      end

      key = begin
        Base64.strict_decode64(secret.delete_prefix("whsec_"))
      rescue ArgumentError
        raise WebhookVerificationError, "webhook secret is not valid base64"
      end
      # Standard Webhooks symmetric keys are 24–64 bytes. A shorter key —
      # especially the zero-byte key from a bare "whsec_" — must never verify.
      unless key.bytesize.between?(24, 64)
        raise WebhookVerificationError, "decoded webhook secret must be 24-64 bytes, got #{key.bytesize}"
      end
      data = "#{msg_id}.#{timestamp}.#{payload}"
      expected = OpenSSL::HMAC.digest("SHA256", key, data)

      signatures.split(" ").each do |candidate|
        version, _, signature = candidate.partition(",")
        next unless version == "v1" && !signature.empty?

        provided = begin
          Base64.strict_decode64(signature)
        rescue ArgumentError
          next
        end
        return nil if provided.bytesize == expected.bytesize &&
                      OpenSSL.fixed_length_secure_compare(provided, expected)
      end
      raise WebhookVerificationError, "no matching v1 signature"
    end
  end
end
